// Copyright 2026 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

//! A wallet for the post-quantum shielded space
//! (docs/internal/privacy-pq-stark.md).
//!
//! The wallet holds the seed and the note openings; the node only ever sees
//! public data. The wallet rebuilds the commitment tree itself from the
//! node's sections (there is no `getTreePath`), finds its notes by their
//! commitments, and proves locally. Payments are delivered out of band: the
//! sender gives the recipient the note opening sealed (`mzx1`, spec §9.2) to
//! the recipient's bundle, over any channel. The channel still shows that
//! the two communicated, never what or how much.
//!
//! ```text
//! (the wallet file is sealed under a passphrase: MAZZE_WALLET_PASSPHRASE, or asked)
//! mazze-shielded-wallet init       --wallet W --rpc URL
//! mazze-shielded-wallet export-seed --wallet W                         (back it up offline)
//! mazze-shielded-wallet address    --wallet W [--index J]
//! mazze-shielded-wallet new-address --wallet W                         (an unlinkable new address)
//! mazze-shielded-wallet export-bundle --wallet W --out FILE [--index J]
//! mazze-shielded-wallet sync       --wallet W --rpc URL
//! mazze-shielded-wallet balance    --wallet W
//! mazze-shielded-wallet shield-in  --wallet W --rpc URL --amount A --funder-key HEX [--espace-rpc URL]
//! mazze-shielded-wallet send       --wallet W --rpc URL --to mzs1.. --amount A
//!                                  --bundle FILE --sealed-out FILE
//! mazze-shielded-wallet receive    --wallet W --opening FILE              (an mzx1 file; offline)
//! mazze-shielded-wallet unshield   --wallet W --rpc URL --to 0xADDR [--espace] --amount A
//! mazze-shielded-wallet claim      --wallet W --rpc URL [--now]
//! Commands that prove also take --verify-rpc URL: a second node that must
//! agree about the anchor's tree, and --hold: keep the proven transaction
//! pending, unsent, until `resubmit` (to prove ahead for a load test).
//! mazze-shielded-wallet restore    --wallet W --rpc URL   (seed asked, or MAZZE_RESTORE_SEED)
//! mazze-shielded-wallet fund-espace --wallet W --rpc URL --funder-key HEX --amount A   (test helper)
//! mazze-shielded-wallet funder-address --rpc URL --funder-key HEX   (the key's native and eSpace addresses)
//! mazze-shielded-wallet resubmit   --wallet W --rpc URL   (re-sends proven txs not yet on chain)
//! mazze-shielded-wallet abandon-pending --wallet W        (drops proven txs that will never land)
//! ```
//!
//! Recovery (spec §9.5): every note the wallet keeps for itself (change,
//! shield-in, claims) is a self-note derived from the seed, with its value
//! sealed in the transaction's recovery slot. `restore` rebuilds them from
//! the seed alone. Notes received from others are recoverable only once
//! claimed into a self-note, so `claim` should run after every receipt.

mod delivery;
mod rpc;
mod store;

use std::{
    collections::{HashMap, HashSet},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use mazze_shielded_pq::{
    air::spend2x2::{SpendIn, Tx2Witness},
    field::{to_bytes, Digest, Sponge, DIGEST, F},
    note::{nullifier, Note},
    ps1::ProverRandomness,
    tree::{Tree, DEPTH},
    wallet::{
        build_transaction, from_shielded_digest, recovery_key, recovery_tag,
        self_index_base, uniform_fields, WalletKeys, GAP_LIMIT, SELF_COUNTER_BITS,
    },
};
use mazze_types::{Address, H160, U256};
use p3_field::{PrimeCharacteristicRing, PrimeField32};
use primitives::{
    shielded::{RecoverySlot, ShieldedDest, ShieldedDigest},
    transaction::{eth_transaction::Eip155Transaction, native_transaction::NativeTransaction},
    Action, Transaction,
};
use rand::RngCore;
use zeroize::Zeroizing;
use rand10::SeedableRng;
use serde_json::{json, Value};

use rpc::{as_u256, as_u64, hex0x, unhex, Node};
use store::{PendingTx, StoredNote, Wallet};

/// The anchor is the tree at the last multiple of this many epochs (about a
/// minute on the fleet): every honest wallet proving in that minute uses
/// the same root, so an anchor cannot tell wallets apart, and a node feeding
/// one wallet its own view of the tree is caught by `getStateAt`.
const ANCHOR_STEP: u64 = 100;

/// A received note is claimed after 10 to 120 minutes.
const CLAIM_DELAY_MIN: u64 = 10 * 60;
const CLAIM_DELAY_SPAN: u64 = 110 * 60;

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// The wallet's chain, fixed when it was created.
fn wallet_sponge(w: &Wallet) -> Result<Sponge, String> {
    w.chain_id
        .map(Sponge::new)
        .ok_or_else(|| "the wallet has no chain id; restore it from its seed".to_string())
}

/// A random self-index prefix not in `taken` (see `SELF_COUNTER_BITS`).
fn fresh_prefix(taken: &HashSet<u16>) -> u16 {
    loop {
        let p = (rand::rngs::OsRng.next_u32() & 0xffff) as u16;
        if !taken.contains(&p) {
            return p;
        }
    }
}

/// Writes a file only its owner can read.
fn write_private(path: &str, data: &[u8]) -> Result<(), String> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .and_then(|mut f| f.write_all(data))
        .map_err(|e| format!("{path}: {e}"))
}

/// Submitting to a remote node over plain HTTP shows the node (and anyone on
/// the path) the IP a transaction came from.
fn warn_if_exposed(url: &str) {
    let local = ["http://127.", "http://localhost", "http://[::1]"].iter().any(|p| url.starts_with(p));
    if !local && !url.contains(".onion") {
        eprintln!(
            "warning: {url} is a remote node: it sees the IP this transaction comes from. \
             Use your own node, or reach it through Tor (torsocks)."
        );
    }
}

/// Mazzy per shielded unit (consensus `SHIELDED_UNIT`, checked against the
/// node): every shielded amount is a whole number of GMazzy.
const UNIT: u64 = 1_000_000_000;

/// A shielded amount in mazzy, for display and for deposits.
fn mazzy(units: u64) -> U256 { U256::from(units) * U256::from(UNIT) }

/// The wallet passphrase: `MAZZE_WALLET_PASSPHRASE`, or asked on the
/// terminal.
fn passphrase() -> Result<Zeroizing<String>, String> {
    // Note: an environment variable is readable by the user's other
    // processes (/proc/<pid>/environ); the prompt is safer.
    if let Ok(p) = std::env::var("MAZZE_WALLET_PASSPHRASE") {
        return Ok(Zeroizing::new(p));
    }
    rpassword::read_password_from_tty(Some("Wallet passphrase: "))
        .map(Zeroizing::new)
        .map_err(|e| e.to_string())
}

/// A passphrase for a new wallet file, typed twice when asked.
fn new_passphrase() -> Result<Zeroizing<String>, String> {
    if let Ok(p) = std::env::var("MAZZE_WALLET_PASSPHRASE") {
        return Ok(Zeroizing::new(p));
    }
    let first = passphrase()?;
    let again = Zeroizing::new(
        rpassword::read_password_from_tty(Some("Again: ")).map_err(|e| e.to_string())?,
    );
    if *first != *again {
        return Err("the passphrases differ".into());
    }
    Ok(first)
}

/// `--index` (default 0): an address this wallet has issued.
fn address_index(w: &Wallet, a: &Args) -> Result<u32, String> {
    let j = match a.0.get("index") {
        Some(v) => v.parse().map_err(|e| format!("--index: {e}"))?,
        None => 0,
    };
    if j >= w.next_address {
        return Err(format!("address {j} not issued yet (use new-address)"));
    }
    Ok(j)
}

/// The cross-space bridge, `0x0888…06`.
const CROSS_SPACE_ADDRESS: [u8; 20] = [
    0x08, 0x88, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x06,
];
/// The shield-in bridge, `0x0888…09`.
const SHIELDED_BRIDGE_ADDRESS: [u8; 20] = [
    0x08, 0x88, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x09,
];

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if let Err(e) = run(&args) {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

struct Args(HashMap<String, String>);

impl Args {
    fn parse(rest: &[String]) -> Result<Self, String> {
        let mut map = HashMap::new();
        let mut it = rest.iter();
        while let Some(key) = it.next() {
            let key = key.strip_prefix("--").ok_or(format!("unexpected argument {key}"))?;
            // Flags without a value.
            if key == "espace" || key == "now" || key == "hold" {
                map.insert(key.into(), "1".into());
                continue;
            }
            let value = it.next().ok_or(format!("--{key} needs a value"))?;
            map.insert(key.into(), value.clone());
        }
        Ok(Args(map))
    }

    fn get(&self, key: &str) -> Result<&str, String> {
        self.0.get(key).map(|s| s.as_str()).ok_or(format!("missing --{key}"))
    }

    /// `--amount` in mazzy.
    fn amount(&self) -> Result<U256, String> {
        U256::from_dec_str(self.get("amount")?).map_err(|e| format!("--amount: {e:?}"))
    }

    /// `--amount` as a shielded amount: a whole number of `UNIT`s.
    fn shielded_amount(&self) -> Result<u64, String> {
        let mazzy = self.amount()?;
        if !(mazzy % U256::from(UNIT)).is_zero() || mazzy / U256::from(UNIT) > U256::from(u64::MAX) {
            return Err(format!("--amount must be a whole number of GMazzy ({UNIT} mazzy)"));
        }
        Ok((mazzy / U256::from(UNIT)).as_u64())
    }
}

fn run(args: &[String]) -> Result<(), String> {
    let command = args.get(1).ok_or("missing command (see the source header)")?;
    let a = Args::parse(&args[2..])?;
    // The one command without a wallet.
    if command == "funder-address" {
        let node = Node::connect(a.get("rpc")?)?;
        let secret = mazzekey::Secret::from_slice(
            &hex::decode(a.get("funder-key")?.trim_start_matches("0x")).map_err(|e| e.to_string())?,
        )
        .ok_or("--funder-key must be a 32-byte secret")?;
        let pair = mazzekey::KeyPair::from_secret(secret).map_err(|e| format!("{e:?}"))?;
        let native = mazze_addr::mazze_addr_encode(
            pair.address().as_bytes(),
            native_network(&node)?,
            mazze_addr::EncodingOptions::Simple,
        )
        .map_err(|e| format!("{e:?}"))?;
        println!("native {native}\nespace {:?}", pair.evm_address());
        return Ok(());
    }
    let path = a.get("wallet")?;
    match command.as_str() {
        "init" => {
            if std::path::Path::new(path).exists() {
                return Err(format!("{path} exists; refusing to overwrite a wallet"));
            }
            let chain = chain_id(&Node::connect(a.get("rpc")?)?)?;
            let mut seed = [0u8; 32];
            rand::rngs::OsRng.fill_bytes(&mut seed);
            let mut w = Wallet::new(seed, &new_passphrase()?)?;
            w.chain_id = Some(chain);
            w.next_self = self_index_base(fresh_prefix(&HashSet::new()));
            w.save(path)?;
            println!("created {path}; back up its seed with export-seed");
            Ok(())
        }
        "export-seed" => {
            let w = Wallet::load(path, passphrase)?;
            println!("{}", hex::encode(w.seed));
            Ok(())
        }
        "address" => {
            let w = Wallet::load(path, passphrase)?;
            let j = address_index(&w, &a)?;
            let sponge = wallet_sponge(&w)?;
            let keys = WalletKeys::from_seed(&sponge, w.seed);
            println!("{}", delivery::address(&keys, &sponge, j).0.encode());
            Ok(())
        }
        "new-address" => {
            // Each address has its own diversifier and its own KEM keys:
            // two addresses of one wallet cannot be linked (spec §3).
            let mut w = Wallet::load(path, passphrase)?;
            let j = w.next_address;
            w.next_address += 1;
            w.save(path)?;
            let sponge = wallet_sponge(&w)?;
            let keys = WalletKeys::from_seed(&sponge, w.seed);
            println!("{j} {}", delivery::address(&keys, &sponge, j).0.encode());
            Ok(())
        }
        "export-bundle" => {
            let w = Wallet::load(path, passphrase)?;
            let j = address_index(&w, &a)?;
            let sponge = wallet_sponge(&w)?;
            let keys = WalletKeys::from_seed(&sponge, w.seed);
            let (_, bundle) = delivery::address(&keys, &sponge, j);
            std::fs::write(a.get("out")?, &bundle.0).map_err(|e| e.to_string())?;
            println!("bundle ({} bytes) written to {}", bundle.0.len(), a.get("out")?);
            Ok(())
        }
        "balance" => {
            let w = Wallet::load(path, passphrase)?;
            let sum = |f: &dyn Fn(&StoredNote) -> bool| -> U256 {
                w.notes.iter().filter(|n| f(n)).fold(U256::zero(), |s, n| s + mazzy(n.value))
            };
            println!(
                "{{\"spendable\": {}, \"pending\": {}, \"notes\": {}}}",
                sum(&|n| n.position.is_some() && !n.spent),
                sum(&|n| n.position.is_none()),
                w.notes.len()
            );
            Ok(())
        }
        "receive" => {
            // Offline: opening a payment tells no node anything.
            let mut w = Wallet::load(path, passphrase)?;
            let sponge = wallet_sponge(&w)?;
            let keys = WalletKeys::from_seed(&sponge, w.seed);
            let text = std::fs::read_to_string(a.get("opening")?).map_err(|e| e.to_string())?;
            if !text.trim_start().starts_with("mzx1") {
                return Err("not a sealed opening (mzx1)".into());
            }
            // Sealed to one of this wallet's addresses, for that address's
            // diversifier.
            let opening = (0..w.next_address)
                .find_map(|j| {
                    let (addr, _) = delivery::address(&keys, &sponge, j);
                    delivery::open(&w.seed, j, &addr, &text).filter(|o| o.d == addr.d)
                })
                .ok_or("the sealed opening is not for this wallet (or was tampered with)")?;
            let note = Note {
                d: opening.d,
                owner: keys.spend.owner(&sponge, &opening.d),
                value: opening.value,
                psi: opening.psi,
                y: opening.y,
            };
            let mut stored = StoredNote::from_note(&note, &sponge);
            if w.notes.iter().any(|n| n.cm == stored.cm) {
                println!("already received");
                return Ok(());
            }
            // Claimed after a random delay, so the sender cannot tell from
            // the timing of the next transaction that it was the claim.
            stored.claim_after = now_secs() + CLAIM_DELAY_MIN + rand::rngs::OsRng.next_u64() % CLAIM_DELAY_SPAN;
            w.notes.push(stored);
            w.save(path)?;
            println!("received {}; it can be claimed after a random delay (claim)", mazzy(opening.value));
            Ok(())
        }
        "sync" => {
            let mut w = Wallet::load(path, passphrase)?;
            let node = Node::connect(a.get("rpc")?)?;
            sync(&mut w, &node)?;
            w.save(path)
        }
        "shield-in" => shield_in(path, &a),
        "claim" => claim(path, &a),
        "restore" => restore(path, &a),
        "send" => send(path, &a),
        "unshield" => unshield(path, &a),
        "resubmit" => resubmit(path, &a),
        "abandon-pending" => {
            // Gives up on proven transactions that will never land (their
            // anchor expired, their deposit was never made): their inputs
            // are spendable again and their outputs are dropped.
            let mut w = Wallet::load(path, passphrase)?;
            let outs: Vec<[u32; DIGEST]> = w.pending.iter().map(|p| p.cm).collect();
            let n = w.pending.len();
            w.pending.clear();
            w.notes.retain(|note| note.position.is_some() || !outs.contains(&note.cm));
            w.save(path)?;
            println!("abandoned {n} pending transactions");
            Ok(())
        }
        "fund-espace" => {
            // A native → eSpace transfer to the same key's eSpace address,
            // through the cross-space bridge (for testing eSpace shield-in).
            let node = Node::connect(a.get("rpc")?)?;
            let secret = mazzekey::Secret::from_slice(
                &hex::decode(a.get("funder-key")?.trim_start_matches("0x")).map_err(|e| e.to_string())?,
            )
            .ok_or("--funder-key must be a 32-byte secret")?;
            let to = mazzekey::KeyPair::from_secret(secret.clone()).map_err(|e| format!("{e:?}"))?.evm_address();
            let mut data = keccak_hash::keccak(b"transferEVM(bytes20)").0[..4].to_vec();
            data.extend_from_slice(to.as_bytes());
            data.extend_from_slice(&[0u8; 12]);
            send_native(&node, &secret, Address::from(CROSS_SPACE_ADDRESS), a.amount()?, data)?;
            println!("funded eSpace {to:?}");
            Ok(())
        }
        other => Err(format!("unknown command {other}")),
    }
}

// ---- keys, notes, randomness ---------------------------------------------

fn entropy() -> [u8; 32] {
    let mut e = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut e);
    e
}

fn fresh<const N: usize>() -> [F; N] { uniform_fields(|| rand::rngs::OsRng.next_u32()) }

fn prover_randomness() -> ProverRandomness {
    ProverRandomness::from_csprng(&mut rand10::rngs::StdRng::from_seed(entropy()))
}

fn chain_id(node: &Node) -> Result<u32, String> {
    Ok(as_u64(&node.call("shielded_getStatus", vec![])?["chainId"])? as u32)
}



/// A note with fresh commitment and nullifier randomness.
fn new_note(d: [F; 3], owner: Digest, value: u64) -> Note {
    Note { d, owner, value, psi: fresh(), y: fresh() }
}

/// A dummy input: value 0, a fresh random key, so its nullifier is random
/// and unlinkable like a real one.
fn dummy_input(sponge: &Sponge) -> SpendIn {
    let nsk: Digest = fresh();
    let keys = mazze_shielded_pq::keys::SpendKeys::from_nsk(sponge, nsk);
    let d = fresh();
    let note = new_note(d, keys.owner(sponge, &d), 0);
    SpendIn { nsk, note, path: [[F::ZERO; DIGEST]; DEPTH], pos: 0 }
}

/// A dummy output: value 0 to a random owner.
fn dummy_output() -> Note { new_note(fresh(), fresh(), 0) }


// ---- sync ---------------------------------------------------------------------

fn parse_digest(v: &Value) -> Result<ShieldedDigest, String> {
    ShieldedDigest::from_bytes(&unhex(v)?).map_err(|e| format!("{e:?}"))
}

/// Rebuilds the tree from the node's sections and updates the notes. It
/// starts a random stretch before where it stopped, so a node cannot tell
/// from the first epoch asked for that two sessions are the same wallet.
fn sync(w: &mut Wallet, node: &Node) -> Result<(), String> {
    match sync_once(w, node) {
        // The node's history moved under what this wallet had (a reorg
        // deeper than the overlap): rebuild the tree from the start once,
        // instead of staying stuck.
        Err(e) if e.contains("history changed") || e.contains("tree gap") => {
            eprintln!("{e}; rescanning from the start");
            reset_tree(w);
            sync_once(w, node)
        }
        r => r,
    }
}

/// Forgets the tree and where notes sit in it; the next sync rebuilds them.
fn reset_tree(w: &mut Wallet) {
    w.leaves.clear();
    w.leaf_epochs.clear();
    w.synced_epoch = 0;
    for note in &mut w.notes {
        note.position = None;
        note.spent = false;
    }
}

fn sync_once(w: &mut Wallet, node: &Node) -> Result<(), String> {
    let chain = chain_id(node)?;
    if w.chain_id.map_or(false, |c| c != chain) {
        return Err(format!("the node serves chain {chain}, not this wallet's"));
    }
    w.chain_id = Some(chain);
    let sponge = Sponge::new(chain);
    let keys = WalletKeys::from_seed(&sponge, w.seed);
    if w.leaf_epochs.len() != w.leaves.len() {
        // A wallet from before leaf epochs were kept: rebuild the tree.
        reset_tree(w);
    }
    let status = node.call("shielded_getStatus", vec![])?;
    let executed = as_u64(&status["executedEpoch"])?;
    let mut from = w.synced_epoch.saturating_sub(rand::rngs::OsRng.next_u64() % 200);
    while from <= executed {
        let sections = node.call(
            "shielded_getSections",
            vec![json!(format!("0x{:x}", from)), json!("0x3e8")],
        )?;
        for section in sections.as_array().ok_or("sections")? {
            let epoch = as_u64(&section["epoch"])?;
            for tx in section["transactions"].as_array().ok_or("transactions")? {
                if tx["applied"] != Value::Bool(true) {
                    continue;
                }
                let positions = tx["positions"].as_array().ok_or("positions")?;
                for k in 0..2 {
                    let pos = as_u64(&positions[k])?;
                    let cm = parse_digest(&tx["outputCommitments"][k])?;
                    let leaf = from_shielded_digest(&cm).map(|x| x.as_canonical_u32());
                    if (pos as usize) < w.leaves.len() {
                        // Overlap with what is already synced: must agree.
                        if w.leaves[pos as usize] != leaf {
                            return Err(format!("the node's history changed at position {pos}"));
                        }
                        continue;
                    }
                    if pos != w.leaves.len() as u64 {
                        return Err(format!("tree gap: position {pos}, have {}", w.leaves.len()));
                    }
                    w.leaves.push(leaf);
                    w.leaf_epochs.push(epoch);
                    w.pending.retain(|p| p.cm != leaf);
                    for note in w.notes.iter_mut().filter(|n| n.position.is_none()) {
                        if note.cm == leaf {
                            note.position = Some(pos);
                        }
                    }
                }
                let spent: Vec<ShieldedDigest> = (0..2)
                    .map(|k| parse_digest(&tx["nullifiers"][k]))
                    .collect::<Result<_, _>>()?;
                for note in w.notes.iter_mut().filter(|n| n.position.is_some() && !n.spent) {
                    let nf = nullifier(&sponge, &keys.spend.nk, &note.psi(), note.position.unwrap());
                    if spent.iter().any(|s| from_shielded_digest(s) == nf) {
                        note.spent = true;
                    }
                }
            }
        }
        from = from.saturating_add(1000).min(executed + 1);
    }
    w.synced_epoch = executed + 1;
    let spendable: u64 =
        w.notes.iter().filter(|n| n.position.is_some() && !n.spent).map(|n| n.value).sum();
    println!(
        "synced to epoch {executed}: {} leaves, spendable {}",
        w.leaves.len(),
        mazzy(spendable)
    );
    Ok(())
}

// ---- transactions -----------------------------------------------------------

struct Context {
    node: Node,
    chain: u32,
    sponge: Sponge,
    keys: WalletKeys,
    fee: u64,
    /// The tree at the anchor epoch, and how many leaves it has: only
    /// notes below that are spendable now.
    anchor: Tree,
    anchor_leaves: u64,
    /// `--hold`: prove and keep the transaction pending, unsent; `resubmit`
    /// sends it (load tests prove ahead, then submit in a burst).
    hold: bool,
}

/// The canonical anchor (see `ANCHOR_STEP`): the tree at the last boundary
/// epoch, checked against the state of the node and of every `--verify-rpc`
/// node at that epoch.
fn anchor_at(w: &Wallet, executed: u64, nodes: &[&Node]) -> Result<(Tree, u64), String> {
    let epoch = executed / ANCHOR_STEP * ANCHOR_STEP;
    let n = w.leaf_epochs.partition_point(|e| *e <= epoch);
    let tree = Tree { leaves: w.leaves[..n].iter().map(|l| l.map(F::new)).collect() };
    let root = to_bytes(&tree.root());
    for node in nodes {
        let state = node.call("shielded_getStateAt", vec![json!(format!("0x{epoch:x}"))])?;
        if unhex(&state["root"])? != root || as_u64(&state["leaves"])? != n as u64 {
            return Err(format!(
                "a node disagrees with this wallet about the tree at epoch {epoch}; \
                 not proving against it"
            ));
        }
    }
    Ok((tree, n as u64))
}

fn context(w: &mut Wallet, a: &Args) -> Result<Context, String> {
    warn_if_exposed(a.get("rpc")?);
    let node = Node::connect(a.get("rpc")?)?;
    sync(w, &node)?;
    let status = node.call("shielded_getStatus", vec![])?;
    if status["active"] != Value::Bool(true) {
        return Err("the shielded space is not active on this node".into());
    }
    let chain = as_u64(&status["chainId"])? as u32;
    let sponge = Sponge::new(chain);
    let keys = WalletKeys::from_seed(&sponge, w.seed);
    if as_u256(&status["amountUnit"]).ok() != Some(U256::from(UNIT)) {
        return Err(format!("the node's shielded unit is not {UNIT} mazzy"));
    }
    let fee = (as_u256(&status["txFee"])? / U256::from(UNIT)).as_u64();
    let executed = as_u64(&status["executedEpoch"])?;
    let verify = match a.get("verify-rpc") {
        Ok(url) => Some(Node::connect(url)?),
        Err(_) => None,
    };
    let mut nodes = vec![&node];
    nodes.extend(verify.as_ref());
    let (anchor, anchor_leaves) = anchor_at(w, executed, &nodes)?;
    let hold = a.0.contains_key("hold");
    Ok(Context { node, chain, sponge, keys, fee, anchor, anchor_leaves, hold })
}

/// Whether a pending transaction already spends note `i`.
fn reserved(w: &Wallet, i: usize) -> bool {
    w.pending.iter().any(|p| p.spends.contains(&w.notes[i].cm))
}

/// Picks up to two unspent notes worth at least `need`, largest first,
/// among those inside the anchor's tree and not reserved by a pending
/// transaction.
fn select_inputs(ctx: &Context, w: &Wallet, need: u64) -> Result<Vec<usize>, String> {
    let mut idx: Vec<usize> = (0..w.notes.len())
        .filter(|&i| {
            w.notes[i].position.map_or(false, |p| p < ctx.anchor_leaves)
                && !w.notes[i].spent
                && !reserved(w, i)
        })
        .collect();
    idx.sort_by_key(|&i| std::cmp::Reverse(w.notes[i].value));
    idx.truncate(2);
    let total = idx
        .iter()
        .try_fold(0u64, |s, &i| s.checked_add(w.notes[i].value))
        .ok_or("note values overflow")?;
    if total < need {
        return Err(format!(
            "spendable {} in two notes is below {} (a note received in the last ~{ANCHOR_STEP} \
             epochs becomes spendable at the next anchor)",
            mazzy(total),
            mazzy(need)
        ));
    }
    Ok(idx)
}

fn spend_input(ctx: &Context, w: &Wallet, tree: &Tree, i: usize) -> SpendIn {
    let n = &w.notes[i];
    let d = n.d();
    let pos = n.position.unwrap();
    SpendIn {
        nsk: ctx.keys.spend.nsk,
        note: Note { d, owner: ctx.keys.spend.owner(&ctx.sponge, &d), value: n.value, psi: n.psi(), y: n.y() },
        path: tree.path(pos),
        pos,
    }
}

/// Proves a transaction.
fn prove(
    ctx: &Context, mut witness: Tx2Witness, dest: Option<ShieldedDest>,
    recovery: RecoverySlot,
) -> Result<(primitives::shielded::ShieldedTxBody, Vec<u8>), String> {
    eprintln!("proving…");
    let (body, proof) =
        build_transaction(ctx.chain, &mut witness, dest, recovery, prover_randomness)?;
    Ok((body, proof))
}

fn submit(ctx: &Context, body: &primitives::shielded::ShieldedTxBody, proof: &[u8]) -> Result<(), String> {
    if ctx.hold {
        println!("proven and held: {:?} (resubmit sends it)", body.hash());
        return Ok(());
    }
    let hash = ctx.node.call(
        "shielded_sendRawTransaction",
        vec![hex0x(&rlp::encode(body)), hex0x(proof)],
    )?;
    println!("submitted shielded transaction {hash}");
    Ok(())
}

/// The next self-note, its recovery slot, and its stored form. The index
/// is reserved and saved before anything is proven: a crash must never
/// let two notes share an index, which would link them by their tag.
fn self_note(
    ctx: &Context, w: &mut Wallet, path: &str, value: u64,
) -> Result<(Note, RecoverySlot, StoredNote), String> {
    let i = w.next_self;
    let counter_mask = (1u32 << SELF_COUNTER_BITS) - 1;
    w.next_self = if (i + 1) & counter_mask == 0 {
        // This prefix is used up: move to a new random one.
        self_index_base(fresh_prefix(&HashSet::new()))
    } else {
        i + 1
    };
    w.save(path)?;
    let note = ctx.keys.self_note(&ctx.sponge, i, value);
    let mut stored = StoredNote::from_note(&note, &ctx.sponge);
    stored.self_index = Some(i);
    Ok((note, ctx.keys.seal_recovery(i, value), stored))
}

/// Saves a proven transaction and the wallet's output before any network
/// side effect (a deposit, a submission), so `resubmit` can always finish it.
fn record_pending(
    w: &mut Wallet, path: &str, body: &primitives::shielded::ShieldedTxBody,
    proof: &[u8], stored: StoredNote, inputs: &[usize],
) -> Result<(), String> {
    let spends = inputs.iter().map(|&i| w.notes[i].cm).collect();
    w.pending.push(PendingTx {
        body: hex::encode(rlp::encode(body)),
        proof: hex::encode(proof),
        cm: stored.cm,
        spends,
    });
    w.notes.push(stored);
    w.save(path)
}

/// Submits again every proven transaction not yet on chain.
fn resubmit(path: &str, a: &Args) -> Result<(), String> {
    let mut w = Wallet::load(path, passphrase)?;
    let node = Node::connect(a.get("rpc")?)?;
    sync(&mut w, &node)?;
    w.save(path)?;
    for p in &w.pending {
        let r = node.call(
            "shielded_sendRawTransaction",
            vec![json!(format!("0x{}", p.body)), json!(format!("0x{}", p.proof))],
        );
        match r {
            Ok(hash) => println!("resubmitted {hash}"),
            Err(e) => println!("not accepted: {e}"),
        }
    }
    Ok(())
}

/// Random output order, so the change is not recognisable by position.
fn shuffle_outputs(a: Note, b: Note) -> [Note; 2] {
    if rand::rngs::OsRng.next_u32() & 1 == 0 { [a, b] } else { [b, a] }
}

fn shield_in(path: &str, a: &Args) -> Result<(), String> {
    let mut w = Wallet::load(path, passphrase)?;
    let ctx = context(&mut w, a)?;
    let amount = a.shielded_amount()?;
    if amount <= ctx.fee {
        return Err(format!("the amount must exceed the fee {}", mazzy(ctx.fee)));
    }
    let (mine, recovery, stored) = self_note(&ctx, &mut w, path, amount - ctx.fee)?;
    let witness = Tx2Witness {
        inputs: [dummy_input(&ctx.sponge), dummy_input(&ctx.sponge)],
        outputs: shuffle_outputs(mine, dummy_output()),
        v_in: amount,
        v_out: 0,
        fee: ctx.fee,
        sighash: [0; 32],
        anchor: ctx.anchor.root(),
        out_limb_override: [None, None],
    };
    let (body, proof) = prove(&ctx, witness, None, recovery)?;
    record_pending(&mut w, path, &body, &proof, stored, &[])?;

    // Before any value moves: the node must accept this body in every
    // respect but the missing deposit. An anchor it no longer accepts, or
    // anything else, is found now, not after the deposit is stranded.
    match ctx.node.call(
        "shielded_sendRawTransaction",
        vec![hex0x(&rlp::encode(&body)), hex0x(&proof)],
    ) {
        Err(e) if e.contains("NoDeposit") => {}
        Ok(_) => {}
        Err(e) => {
            w.pending.pop();
            w.notes.pop();
            w.save(path)?;
            return Err(format!("the node would refuse this shield-in ({e}); no deposit was made"));
        }
    }

    // The deposit: a signed call to the bridge with the amount, from native
    // or, with --espace-rpc, from eSpace (the same key, its eSpace address).
    let secret = mazzekey::Secret::from_slice(
        &hex::decode(a.get("funder-key")?.trim_start_matches("0x")).map_err(|e| e.to_string())?,
    )
    .ok_or("--funder-key must be a 32-byte secret")?;
    let mut data = keccak_hash::keccak(b"shieldIn(bytes32)").0[..4].to_vec();
    data.extend_from_slice(body.hash().as_bytes());
    let bridge = Address::from(SHIELDED_BRIDGE_ADDRESS);
    match a.get("espace-rpc") {
        Ok(url) => send_espace(&Node::connect(url)?, &secret, bridge, mazzy(amount), data)?,
        Err(_) => send_native(&ctx.node, &secret, bridge, mazzy(amount), data)?,
    };
    // The bridge's credit is the authority, not the receipt: in a DAG the
    // same transaction can be packed twice, and the receipt returned may be
    // the skipped duplicate's.
    // The node may still be executing the epoch of the deposit.
    let mut credit = U256::zero();
    for _ in 0..30 {
        credit = deposit_credit(&ctx.node, body.hash().as_bytes())?;
        if credit >= mazzy(amount) {
            break;
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    if credit < mazzy(amount) {
        return Err(format!(
            "the bridge holds a credit of {credit} for this body, expected {}; the body is saved: \
             run resubmit once the deposit is executed",
            mazzy(amount)
        ));
    }
    println!("deposit credited: {credit}");
    submit(&ctx, &body, &proof)?;
    Ok(())
}

fn send(path: &str, a: &Args) -> Result<(), String> {
    let mut w = Wallet::load(path, passphrase)?;
    let ctx = context(&mut w, a)?;
    let amount = a.shielded_amount()?;
    let to = delivery::Address::decode(a.get("to")?)
        .ok_or("--to is not a valid mzs1 address (check for typos)")?;
    let (d, owner) = (to.d, to.owner);
    // Everything the delivery needs is checked before anything is proven or
    // sent: a payment whose opening cannot be delivered is lost.
    let bundle = delivery::Bundle(std::fs::read(a.get("bundle")?).map_err(|e| e.to_string())?);
    let sealed_out = a.get("sealed-out")?.to_string();
    let need = amount.checked_add(ctx.fee).ok_or("amount overflow")?;
    let inputs = select_inputs(&ctx, &w, need)?;
    let total = inputs
        .iter()
        .try_fold(0u64, |s, &i| s.checked_add(w.notes[i].value))
        .ok_or("note values overflow")?;
    let (change, recovery, stored) = self_note(&ctx, &mut w, path, total - need)?;
    let t = &ctx.anchor;
    let input = |k: usize| inputs.get(k).map(|&i| spend_input(&ctx, &w, t, i)).unwrap_or_else(|| dummy_input(&ctx.sponge));
    let theirs = new_note(d, owner, amount);
    let opening = delivery::Opening { d: theirs.d, value: theirs.value, psi: theirs.psi, y: theirs.y };
    let sealed = delivery::seal(&to, &bundle, &opening)?;
    write_private(&sealed_out, sealed.as_bytes())?;
    let witness = Tx2Witness {
        inputs: [input(0), input(1)],
        outputs: shuffle_outputs(theirs.clone(), change),
        v_in: 0,
        v_out: 0,
        fee: ctx.fee,
        sighash: [0; 32],
        anchor: ctx.anchor.root(),
        out_limb_override: [None, None],
    };
    let (body, proof) = prove(&ctx, witness, None, recovery)?;
    record_pending(&mut w, path, &body, &proof, stored, &inputs)?;
    submit(&ctx, &body, &proof)?;
    println!(
        "sealed opening written to {sealed_out}; deliver it to the recipient once the transaction \
         is on chain (sync shows its outputs)"
    );
    Ok(())
}

fn unshield(path: &str, a: &Args) -> Result<(), String> {
    let mut w = Wallet::load(path, passphrase)?;
    let ctx = context(&mut w, a)?;
    let amount = a.shielded_amount()?;
    let to: H160 = a.get("to")?.trim_start_matches("0x").parse().map_err(|e| format!("--to: {e:?}"))?;
    let space = if a.0.contains_key("espace") { mazze_types::Space::Ethereum } else { mazze_types::Space::Native };
    let need = amount.checked_add(ctx.fee).ok_or("amount overflow")?;
    let inputs = select_inputs(&ctx, &w, need)?;
    let total = inputs
        .iter()
        .try_fold(0u64, |s, &i| s.checked_add(w.notes[i].value))
        .ok_or("note values overflow")?;
    let (change, recovery, stored) = self_note(&ctx, &mut w, path, total - need)?;
    let t = &ctx.anchor;
    let input = |k: usize| inputs.get(k).map(|&i| spend_input(&ctx, &w, t, i)).unwrap_or_else(|| dummy_input(&ctx.sponge));
    let witness = Tx2Witness {
        inputs: [input(0), input(1)],
        outputs: shuffle_outputs(change, dummy_output()),
        v_in: 0,
        v_out: amount,
        fee: ctx.fee,
        sighash: [0; 32],
        anchor: ctx.anchor.root(),
        out_limb_override: [None, None],
    };
    let (body, proof) =
        prove(&ctx, witness, Some(ShieldedDest { space, address: to }), recovery)?;
    record_pending(&mut w, path, &body, &proof, stored, &inputs)?;
    submit(&ctx, &body, &proof)?;
    Ok(())
}

/// Claims received notes (spec §9.5): each is spent, together with the
/// wallet's largest self-note, into one new self-note. Afterwards the value
/// is recoverable from the seed and the wallet holds one large note.
fn claim(path: &str, a: &Args) -> Result<(), String> {
    let mut w = Wallet::load(path, passphrase)?;
    let ctx = context(&mut w, a)?;
    let now = now_secs();
    let claimable = |n: &StoredNote| {
        n.self_index.is_none()
            && n.position.map_or(false, |p| p < ctx.anchor_leaves)
            && !n.spent
            && !w.pending.iter().any(|p| p.spends.contains(&n.cm))
    };
    let received = w
        .notes
        .iter()
        .position(|n| claimable(n) && (n.claim_after <= now || a.0.contains_key("now")))
        .ok_or_else(|| match w.notes.iter().filter(|n| claimable(n)).map(|n| n.claim_after).min() {
            Some(t) => format!(
                "the received note can be claimed in {} minutes (a random delay, so its sender \
                 cannot time the claim); --now claims at once",
                (t - now).div_ceil(60)
            ),
            None => "no received note to claim (yet)".to_string(),
        })?;
    let main = (0..w.notes.len())
        .filter(|&i| {
            w.notes[i].self_index.is_some()
                && w.notes[i].position.map_or(false, |p| p < ctx.anchor_leaves)
                && !w.notes[i].spent
                && !reserved(&w, i)
        })
        .max_by_key(|&i| w.notes[i].value);
    let total = w.notes[received]
        .value
        .checked_add(main.map(|i| w.notes[i].value).unwrap_or(0))
        .ok_or("note values overflow")?;
    if total <= ctx.fee {
        return Err(format!("the notes ({}) do not cover the fee {}", mazzy(total), mazzy(ctx.fee)));
    }
    let t = &ctx.anchor;
    let second = main.map(|i| spend_input(&ctx, &w, t, i)).unwrap_or_else(|| dummy_input(&ctx.sponge));
    let (mine, recovery, stored) = self_note(&ctx, &mut w, path, total - ctx.fee)?;
    let witness = Tx2Witness {
        inputs: [spend_input(&ctx, &w, t, received), second],
        outputs: shuffle_outputs(mine, dummy_output()),
        v_in: 0,
        v_out: 0,
        fee: ctx.fee,
        sighash: [0; 32],
        anchor: ctx.anchor.root(),
        out_limb_override: [None, None],
    };
    let (body, proof) = prove(&ctx, witness, None, recovery)?;
    let spends: Vec<usize> = std::iter::once(received).chain(main).collect();
    record_pending(&mut w, path, &body, &proof, stored, &spends)?;
    submit(&ctx, &body, &proof)?;
    Ok(())
}

/// Rebuilds a wallet from the seed alone: scans every section, matches the
/// self-note tags locally (the node never learns which tags are ours),
/// opens their slots and rebuilds the notes. Each device prefix is scanned
/// until `GAP_LIMIT` consecutive missing tags.
fn restore(path: &str, a: &Args) -> Result<(), String> {
    if std::path::Path::new(path).exists() {
        return Err(format!("{path} exists; restore into a new file"));
    }
    // Never from the command line, where it would sit in the shell's
    // history and in every process listing.
    let text = match std::env::var("MAZZE_RESTORE_SEED") {
        Ok(v) => Zeroizing::new(v),
        Err(_) => Zeroizing::new(
            rpassword::read_password_from_tty(Some("Seed (hex): ")).map_err(|e| e.to_string())?,
        ),
    };
    let seed: [u8; 32] = hex::decode(text.trim().trim_start_matches("0x"))
        .map_err(|e| e.to_string())?
        .try_into()
        .map_err(|_| "the seed is 32 bytes")?;
    let node = Node::connect(a.get("rpc")?)?;
    let chain = chain_id(&node)?;
    let sponge = Sponge::new(chain);
    let keys = WalletKeys::from_seed(&sponge, seed);
    let mut w = Wallet::new(seed, &new_passphrase()?)?;
    w.chain_id = Some(chain);

    // Every applied transaction by recovery tag. Tags are public, so anyone
    // may copy one into a transaction of their own: every transaction of a
    // tag is kept, and the one whose slot opens and whose outputs hold the
    // rebuilt note is the wallet's.
    let status = node.call("shielded_getStatus", vec![])?;
    let executed = as_u64(&status["executedEpoch"])?;
    let mut by_tag: HashMap<Vec<u8>, Vec<Value>> = HashMap::new();
    let mut from = 0u64;
    while from <= executed {
        let sections = node.call(
            "shielded_getSections",
            vec![json!(format!("0x{:x}", from)), json!("0x3e8")],
        )?;
        for section in sections.as_array().ok_or("sections")? {
            for tx in section["transactions"].as_array().ok_or("transactions")? {
                if tx["applied"] == Value::Bool(true) {
                    by_tag.entry(unhex(&tx["recoveryTag"])?).or_default().push(tx.clone());
                }
            }
        }
        from = from.saturating_add(1000);
    }

    // Every device prefix, each until GAP_LIMIT consecutive misses.
    let rkey = recovery_key(&seed);
    let mut used = HashSet::new();
    for prefix in 0..=u16::MAX {
        let base = self_index_base(prefix);
        let (mut misses, mut counter) = (0u32, 0u32);
        while misses < GAP_LIMIT && counter < (1 << SELF_COUNTER_BITS) {
            let i = base | counter;
            counter += 1;
            let found = by_tag.get(recovery_tag(&rkey, i).as_slice()).and_then(|txs| {
                txs.iter().find_map(|tx| {
                    let slot = RecoverySlot {
                        tag: recovery_tag(&rkey, i),
                        slot: unhex(&tx["recoverySlot"]).ok()?.try_into().ok()?,
                        mac: unhex(&tx["recoveryMac"]).ok()?.try_into().ok()?,
                    };
                    let value = keys.open_recovery(i, &slot)?;
                    let mut stored = StoredNote::from_note(&keys.self_note(&sponge, i, value), &sponge);
                    let outputs = tx["outputCommitments"].as_array()?;
                    let ours = outputs.iter().any(|cm| {
                        parse_digest(cm)
                            .map(|d| from_shielded_digest(&d).map(|x| x.as_canonical_u32()) == stored.cm)
                            .unwrap_or(false)
                    });
                    ours.then(|| {
                        stored.self_index = Some(i);
                        stored
                    })
                })
            });
            match found {
                Some(stored) => {
                    misses = 0;
                    used.insert(prefix);
                    w.notes.push(stored);
                }
                None => misses += 1,
            }
        }
    }
    // This device continues under a prefix no earlier device used.
    w.next_self = self_index_base(fresh_prefix(&used));
    println!("recovered {} self-notes from the seed", w.notes.len());
    // The ordinary sync rebuilds the tree, places the notes and marks the
    // spent ones.
    sync(&mut w, &node)?;
    w.save(path)
}

// ---- the native deposit ------------------------------------------------------

fn native_network(node: &Node) -> Result<mazze_addr::Network, String> {
    Ok(match as_u64(&node.call("mazze_getStatus", vec![])?["networkId"])? {
        1 => mazze_addr::Network::Test,
        1990 => mazze_addr::Network::Main,
        n => mazze_addr::Network::Id(n),
    })
}

/// The shield-in credit the bridge holds for `body_hash` (`deposit(bytes32)`).
fn deposit_credit(node: &Node, body_hash: &[u8]) -> Result<U256, String> {
    let bridge = mazze_addr::mazze_addr_encode(
        &SHIELDED_BRIDGE_ADDRESS,
        native_network(node)?,
        mazze_addr::EncodingOptions::Simple,
    )
    .map_err(|e| format!("{e:?}"))?;
    let mut data = keccak_hash::keccak(b"deposit(bytes32)").0[..4].to_vec();
    data.extend_from_slice(body_hash);
    let out = node.call("mazze_call", vec![json!({"to": bridge, "data": hex0x(&data)}), json!("latest_state")])?;
    let bytes = unhex(&out)?;
    if bytes.len() != 32 {
        return Err(format!("unexpected deposit() result {out}"));
    }
    Ok(U256::from_big_endian(&bytes))
}

/// Sends a signed eSpace (EIP-155) transaction and waits for its receipt.
fn send_espace(
    node: &Node, secret: &mazzekey::Secret, to: Address, value: U256, data: Vec<u8>,
) -> Result<Value, String> {
    let chain_id = as_u64(&node.call("eth_chainId", vec![])?)? as u32;
    let gas_price = U256::from(as_u64(&node.call("eth_gasPrice", vec![])?)?);
    let build = |nonce: U256| -> primitives::SignedTransaction {
        let tx: Transaction = Eip155Transaction {
            nonce,
            gas_price,
            gas: 300_000.into(),
            action: Action::Call(to),
            value,
            chain_id: Some(chain_id),
            data: data.clone(),
        }
        .into();
        tx.sign(secret)
    };
    let sender = build(U256::zero()).sender().address;
    let nonce = U256::from(as_u64(
        &node.call("eth_getTransactionCount", vec![json!(format!("{sender:?}")), json!("latest")])?,
    )?);
    let signed = build(nonce);
    let hash = node.call("eth_sendRawTransaction", vec![hex0x(&rlp::encode(&signed.transaction))])?;
    eprintln!("eSpace deposit {hash} sent from {sender:?}, waiting for execution…");
    for _ in 0..600 {
        let receipt = node.call("eth_getTransactionReceipt", vec![hash.clone()])?;
        if !receipt.is_null() {
            return Ok(receipt);
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    Err("the deposit was not executed within 5 minutes".into())
}

fn send_native(
    node: &Node, secret: &mazzekey::Secret, to: Address, value: U256, data: Vec<u8>,
) -> Result<Value, String> {
    let status = node.call("mazze_getStatus", vec![])?;
    let chain_id = as_u64(&status["chainId"])? as u32;
    let network = native_network(node)?;
    let epoch = as_u64(&node.call("mazze_epochNumber", vec![json!("latest_state")])?)?;
    let gas_price = U256::from(as_u64(&node.call("mazze_gasPrice", vec![])?)?);
    let build = |nonce: U256| -> primitives::SignedTransaction {
        let tx: Transaction = NativeTransaction {
            nonce,
            gas_price,
            gas: 300_000.into(),
            action: Action::Call(to),
            value,
            storage_limit: 0,
            epoch_height: epoch,
            chain_id,
            data: data.clone(),
        }
        .into();
        tx.sign(secret)
    };
    let sender = build(U256::zero()).sender().address;
    let base32 = mazze_addr::mazze_addr_encode(sender.as_bytes(), network, mazze_addr::EncodingOptions::Simple)
        .map_err(|e| format!("{e:?}"))?;
    let nonce = U256::from(as_u64(&node.call("mazze_getNextNonce", vec![json!(base32)])?)?);
    let signed = build(nonce);
    let hash = node.call("mazze_sendRawTransaction", vec![hex0x(&rlp::encode(&signed.transaction))])?;
    eprintln!("deposit {hash} sent, waiting for execution…");
    for _ in 0..600 {
        let receipt = node.call("mazze_getTransactionReceipt", vec![hash.clone()])?;
        if !receipt.is_null() {
            return Ok(receipt);
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    Err("the deposit was not executed within 5 minutes".into())
}
