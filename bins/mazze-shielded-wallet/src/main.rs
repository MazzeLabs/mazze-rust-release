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
//! sender gives the recipient the note opening, plain (`mzn1`) over a
//! private channel, or sealed (`mzx1`, spec §9.2) to the recipient's bundle
//! over any channel.
//!
//! ```text
//! mazze-shielded-wallet init       --wallet W
//! mazze-shielded-wallet address    --wallet W --rpc URL
//! mazze-shielded-wallet export-bundle --wallet W --rpc URL --out FILE
//! mazze-shielded-wallet sync       --wallet W --rpc URL
//! mazze-shielded-wallet balance    --wallet W
//! mazze-shielded-wallet shield-in  --wallet W --rpc URL --amount A --funder-key HEX
//! mazze-shielded-wallet send       --wallet W --rpc URL --to mzs1.. --amount A
//!                                  (--bundle FILE --sealed-out FILE | --opening-out FILE)
//! mazze-shielded-wallet receive    --wallet W --rpc URL --opening FILE   (mzn1 or mzx1)
//! mazze-shielded-wallet unshield   --wallet W --rpc URL --to 0xADDR [--espace] --amount A
//! mazze-shielded-wallet claim      --wallet W --rpc URL
//! mazze-shielded-wallet restore    --wallet W --rpc URL --seed HEX
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

use std::{collections::HashMap, time::Duration};

use mazze_shielded_pq::{
    air::spend2x2::{SpendIn, Tx2Witness},
    field::{Digest, Sponge, DIGEST, F},
    note::{nullifier, Note},
    ps1::ProverRandomness,
    tree::{Tree, DEPTH},
    wallet::{
        build_transaction, fresh_fields, from_shielded_digest, recovery_key,
        recovery_tag, WalletKeys, GAP_LIMIT, MAX_SELF_INDEX,
    },
};
use mazze_types::{Address, H160, U256};
use p3_field::{PrimeCharacteristicRing, PrimeField32};
use primitives::{
    shielded::{RecoverySlot, ShieldedDest, ShieldedDigest},
    transaction::native_transaction::NativeTransaction,
    Action, Transaction,
};
use rand::RngCore;
use rand10::SeedableRng;
use serde_json::{json, Value};

use rpc::{as_u64, hex0x, unhex, Node};
use store::{StoredNote, Wallet};

/// The native-space account of the shield-in bridge (0x0888…09).
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
            if key == "espace" {
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

    fn amount(&self) -> Result<u64, String> {
        self.get("amount")?.parse().map_err(|e| format!("--amount: {e}"))
    }
}

fn run(args: &[String]) -> Result<(), String> {
    let command = args.get(1).ok_or("missing command (see the source header)")?;
    let a = Args::parse(&args[2..])?;
    let path = a.get("wallet")?;
    match command.as_str() {
        "init" => {
            let mut seed = [0u8; 32];
            rand::rngs::OsRng.fill_bytes(&mut seed);
            Wallet::new(seed).save(path)?;
            println!("created {path}");
            Ok(())
        }
        "address" => {
            let w = Wallet::load(path)?;
            let sponge = Sponge::new(chain_id(&Node::connect(a.get("rpc")?)?)?);
            let keys = WalletKeys::from_seed(&sponge, w.seed);
            println!("{}", delivery::address(&keys, &sponge, 0).0.encode());
            Ok(())
        }
        "export-bundle" => {
            let w = Wallet::load(path)?;
            let sponge = Sponge::new(chain_id(&Node::connect(a.get("rpc")?)?)?);
            let keys = WalletKeys::from_seed(&sponge, w.seed);
            let (_, bundle) = delivery::address(&keys, &sponge, 0);
            std::fs::write(a.get("out")?, &bundle.0).map_err(|e| e.to_string())?;
            println!("bundle ({} bytes) written to {}", bundle.0.len(), a.get("out")?);
            Ok(())
        }
        "balance" => {
            let w = Wallet::load(path)?;
            let spendable: u64 = w.notes.iter().filter(|n| n.position.is_some() && !n.spent).map(|n| n.value).sum();
            let pending: u64 = w.notes.iter().filter(|n| n.position.is_none()).map(|n| n.value).sum();
            println!("{{\"spendable\": {spendable}, \"pending\": {pending}, \"notes\": {}}}", w.notes.len());
            Ok(())
        }
        "receive" => {
            let mut w = Wallet::load(path)?;
            let sponge = Sponge::new(chain_id(&Node::connect(a.get("rpc")?)?)?);
            let keys = WalletKeys::from_seed(&sponge, w.seed);
            let text = std::fs::read_to_string(a.get("opening")?).map_err(|e| e.to_string())?;
            let opening = if text.trim_start().starts_with("mzx1") {
                let (addr, _) = delivery::address(&keys, &sponge, 0);
                delivery::open(&w.seed, 0, &addr.bundle_hash, &text)
                    .ok_or("the sealed opening is not for this wallet (or was tampered with)")?
            } else {
                delivery::Opening::from_plain(&text).ok_or("not an mzn1 or mzx1 opening")?
            };
            let note = Note {
                d: opening.d,
                owner: keys.spend.owner(&sponge, &opening.d),
                value: opening.value,
                psi: opening.psi,
                y: opening.y,
            };
            w.notes.push(StoredNote::from_note(&note, &sponge));
            w.save(path)?;
            println!("received {}; run sync, then claim it", opening.value);
            Ok(())
        }
        "sync" => {
            let mut w = Wallet::load(path)?;
            let node = Node::connect(a.get("rpc")?)?;
            sync(&mut w, &node)?;
            w.save(path)
        }
        "shield-in" => shield_in(path, &a),
        "claim" => claim(path, &a),
        "restore" => restore(path, &a),
        "send" => send(path, &a),
        "unshield" => unshield(path, &a),
        other => Err(format!("unknown command {other}")),
    }
}

// ---- keys, notes, randomness ---------------------------------------------

fn entropy() -> [u8; 32] {
    let mut e = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut e);
    e
}

fn fresh<const N: usize>() -> [F; N] { fresh_fields(&entropy()) }

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

/// Rebuilds the tree from the node's sections and updates the notes.
fn sync(w: &mut Wallet, node: &Node) -> Result<(), String> {
    let chain = chain_id(node)?;
    let sponge = Sponge::new(chain);
    let keys = WalletKeys::from_seed(&sponge, w.seed);
    let status = node.call("shielded_getStatus", vec![])?;
    let executed = as_u64(&status["executedEpoch"])?;
    while w.synced_epoch <= executed {
        let sections = node.call(
            "shielded_getSections",
            vec![json!(format!("0x{:x}", w.synced_epoch)), json!("0x3e8")],
        )?;
        for section in sections.as_array().ok_or("sections")? {
            for tx in section["transactions"].as_array().ok_or("transactions")? {
                if tx["applied"] != Value::Bool(true) {
                    continue;
                }
                let positions = tx["positions"].as_array().ok_or("positions")?;
                for k in 0..2 {
                    let pos = as_u64(&positions[k])?;
                    let cm = parse_digest(&tx["outputCommitments"][k])?;
                    if pos != w.leaves.len() as u64 {
                        return Err(format!("tree gap: position {pos}, have {}", w.leaves.len()));
                    }
                    w.leaves.push(from_shielded_digest(&cm).map(|x| x.as_canonical_u32()));
                    for note in w.notes.iter_mut().filter(|n| n.position.is_none()) {
                        if note.cm == w.leaves[pos as usize] {
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
        w.synced_epoch = w.synced_epoch.saturating_add(1000).min(executed + 1);
    }
    let spendable: u64 =
        w.notes.iter().filter(|n| n.position.is_some() && !n.spent).map(|n| n.value).sum();
    println!(
        "synced to epoch {executed}: {} leaves, spendable {spendable}",
        w.leaves.len()
    );
    Ok(())
}

fn tree(w: &Wallet) -> Tree {
    Tree { leaves: w.leaves.iter().map(|l| l.map(F::new)).collect() }
}

// ---- transactions -----------------------------------------------------------

struct Context {
    node: Node,
    chain: u32,
    sponge: Sponge,
    keys: WalletKeys,
    fee: u64,
}

fn context(w: &mut Wallet, a: &Args) -> Result<Context, String> {
    let node = Node::connect(a.get("rpc")?)?;
    sync(w, &node)?;
    let status = node.call("shielded_getStatus", vec![])?;
    if status["active"] != Value::Bool(true) {
        return Err("the shielded space is not active on this node".into());
    }
    let chain = as_u64(&status["chainId"])? as u32;
    let sponge = Sponge::new(chain);
    let keys = WalletKeys::from_seed(&sponge, w.seed);
    let fee = as_u64(&status["txFee"])?;
    Ok(Context { node, chain, sponge, keys, fee })
}

/// Picks up to two unspent notes worth at least `need`, largest first.
fn select_inputs(w: &Wallet, need: u64) -> Result<Vec<usize>, String> {
    let mut idx: Vec<usize> = (0..w.notes.len())
        .filter(|&i| w.notes[i].position.is_some() && !w.notes[i].spent)
        .collect();
    idx.sort_by_key(|&i| std::cmp::Reverse(w.notes[i].value));
    idx.truncate(2);
    let total: u64 = idx.iter().map(|&i| w.notes[i].value).sum();
    if total < need {
        return Err(format!("spendable {total} in two notes is below {need}"));
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

/// Proves and submits; returns the body hash.
fn prove_and_submit(
    ctx: &Context, mut witness: Tx2Witness, dest: Option<ShieldedDest>,
    recovery: RecoverySlot,
) -> Result<(primitives::shielded::ShieldedTxBody, Vec<u8>), String> {
    eprintln!("proving…");
    let (body, proof) =
        build_transaction(ctx.chain, &mut witness, dest, recovery, prover_randomness)?;
    Ok((body, proof))
}

fn submit(ctx: &Context, body: &primitives::shielded::ShieldedTxBody, proof: &[u8]) -> Result<(), String> {
    let hash = ctx.node.call(
        "shielded_sendRawTransaction",
        vec![hex0x(&rlp::encode(body)), hex0x(proof)],
    )?;
    println!("submitted shielded transaction {hash}");
    Ok(())
}

/// The next self-note, its recovery slot, and its stored form. The index
/// advances only when the wallet is saved after a successful submission.
fn self_note(ctx: &Context, w: &Wallet, value: u64) -> (Note, RecoverySlot, StoredNote) {
    let i = w.next_self;
    let note = ctx.keys.self_note(&ctx.sponge, i, value);
    let mut stored = StoredNote::from_note(&note, &ctx.sponge);
    stored.self_index = Some(i);
    (note, ctx.keys.seal_recovery(i, value), stored)
}

/// Random output order, so the change is not recognisable by position.
fn shuffle_outputs(a: Note, b: Note) -> [Note; 2] {
    if rand::rngs::OsRng.next_u32() & 1 == 0 { [a, b] } else { [b, a] }
}

fn shield_in(path: &str, a: &Args) -> Result<(), String> {
    let mut w = Wallet::load(path)?;
    let ctx = context(&mut w, a)?;
    let amount = a.amount()?;
    if amount <= ctx.fee {
        return Err(format!("the amount must exceed the fee {}", ctx.fee));
    }
    let (mine, recovery, stored) = self_note(&ctx, &w, amount - ctx.fee);
    let witness = Tx2Witness {
        inputs: [dummy_input(&ctx.sponge), dummy_input(&ctx.sponge)],
        outputs: shuffle_outputs(mine, dummy_output()),
        v_in: amount,
        v_out: 0,
        fee: ctx.fee,
        sighash: [0; 32],
        anchor: tree(&w).root(),
        out_limb_override: [None, None],
    };
    let (body, proof) = prove_and_submit(&ctx, witness, None, recovery)?;

    // The deposit: a signed native call to the bridge with the amount.
    let secret = mazzekey::Secret::from_slice(
        &hex::decode(a.get("funder-key")?.trim_start_matches("0x")).map_err(|e| e.to_string())?,
    )
    .ok_or("--funder-key must be a 32-byte secret")?;
    let mut data = keccak_hash::keccak(b"shieldIn(bytes32)").0[..4].to_vec();
    data.extend_from_slice(body.hash().as_bytes());
    send_native(&ctx.node, &secret, Address::from(SHIELDED_BRIDGE_ADDRESS), amount, data)?;
    // The bridge's credit is the authority, not the receipt: in a DAG the
    // same transaction can be packed twice, and the receipt returned may be
    // the skipped duplicate's.
    let credit = deposit_credit(&ctx.node, body.hash().as_bytes())?;
    if credit != amount {
        return Err(format!("the bridge holds a credit of {credit} for this body, expected {amount}"));
    }
    println!("deposit credited: {credit}");
    submit(&ctx, &body, &proof)?;
    w.notes.push(stored);
    w.next_self += 1;
    w.save(path)
}

fn send(path: &str, a: &Args) -> Result<(), String> {
    let mut w = Wallet::load(path)?;
    let ctx = context(&mut w, a)?;
    let amount = a.amount()?;
    let to = delivery::Address::decode(a.get("to")?).ok_or("--to is not an mzs1 address")?;
    let (d, owner) = (to.d, to.owner);
    let inputs = select_inputs(&w, amount + ctx.fee)?;
    let total: u64 = inputs.iter().map(|&i| w.notes[i].value).sum();
    let t = tree(&w);
    let input = |k: usize| inputs.get(k).map(|&i| spend_input(&ctx, &w, &t, i)).unwrap_or_else(|| dummy_input(&ctx.sponge));
    let theirs = new_note(d, owner, amount);
    let (change, recovery, stored) = self_note(&ctx, &w, total - amount - ctx.fee);
    let witness = Tx2Witness {
        inputs: [input(0), input(1)],
        outputs: shuffle_outputs(theirs.clone(), change),
        v_in: 0,
        v_out: 0,
        fee: ctx.fee,
        sighash: [0; 32],
        anchor: t.root(),
        out_limb_override: [None, None],
    };
    let (body, proof) = prove_and_submit(&ctx, witness, None, recovery)?;
    submit(&ctx, &body, &proof)?;
    let opening = delivery::Opening { d: theirs.d, value: theirs.value, psi: theirs.psi, y: theirs.y };
    if let Ok(bundle_file) = a.get("bundle") {
        let bundle = delivery::Bundle(std::fs::read(bundle_file).map_err(|e| e.to_string())?);
        let sealed = delivery::seal(&to, &bundle, &opening)?;
        std::fs::write(a.get("sealed-out")?, &sealed).map_err(|e| e.to_string())?;
        println!("sealed opening written to {}", a.get("sealed-out")?);
    } else {
        std::fs::write(a.get("opening-out")?, opening.to_plain()).map_err(|e| e.to_string())?;
        println!("plain opening written to {} (use only over a private channel)", a.get("opening-out")?);
    }
    w.notes.push(stored);
    w.next_self += 1;
    w.save(path)
}

fn unshield(path: &str, a: &Args) -> Result<(), String> {
    let mut w = Wallet::load(path)?;
    let ctx = context(&mut w, a)?;
    let amount = a.amount()?;
    let to: H160 = a.get("to")?.trim_start_matches("0x").parse().map_err(|e| format!("--to: {e:?}"))?;
    let space = if a.0.contains_key("espace") { mazze_types::Space::Ethereum } else { mazze_types::Space::Native };
    let inputs = select_inputs(&w, amount + ctx.fee)?;
    let total: u64 = inputs.iter().map(|&i| w.notes[i].value).sum();
    let t = tree(&w);
    let input = |k: usize| inputs.get(k).map(|&i| spend_input(&ctx, &w, &t, i)).unwrap_or_else(|| dummy_input(&ctx.sponge));
    let (change, recovery, stored) = self_note(&ctx, &w, total - amount - ctx.fee);
    let witness = Tx2Witness {
        inputs: [input(0), input(1)],
        outputs: shuffle_outputs(change, dummy_output()),
        v_in: 0,
        v_out: amount,
        fee: ctx.fee,
        sighash: [0; 32],
        anchor: t.root(),
        out_limb_override: [None, None],
    };
    let (body, proof) =
        prove_and_submit(&ctx, witness, Some(ShieldedDest { space, address: to }), recovery)?;
    submit(&ctx, &body, &proof)?;
    w.notes.push(stored);
    w.next_self += 1;
    w.save(path)
}

/// Claims received notes (spec §9.5): each is spent, together with the
/// wallet's largest self-note, into one new self-note. Afterwards the value
/// is recoverable from the seed and the wallet holds one large note.
fn claim(path: &str, a: &Args) -> Result<(), String> {
    let mut w = Wallet::load(path)?;
    let ctx = context(&mut w, a)?;
    let received = w
        .notes
        .iter()
        .position(|n| n.self_index.is_none() && n.position.is_some() && !n.spent)
        .ok_or("no received note to claim")?;
    let main = (0..w.notes.len())
        .filter(|&i| w.notes[i].self_index.is_some() && w.notes[i].position.is_some() && !w.notes[i].spent)
        .max_by_key(|&i| w.notes[i].value);
    let total = w.notes[received].value + main.map(|i| w.notes[i].value).unwrap_or(0);
    if total <= ctx.fee {
        return Err(format!("the notes ({total}) do not cover the fee {}", ctx.fee));
    }
    let t = tree(&w);
    let second = main.map(|i| spend_input(&ctx, &w, &t, i)).unwrap_or_else(|| dummy_input(&ctx.sponge));
    let (mine, recovery, stored) = self_note(&ctx, &w, total - ctx.fee);
    let witness = Tx2Witness {
        inputs: [spend_input(&ctx, &w, &t, received), second],
        outputs: shuffle_outputs(mine, dummy_output()),
        v_in: 0,
        v_out: 0,
        fee: ctx.fee,
        sighash: [0; 32],
        anchor: t.root(),
        out_limb_override: [None, None],
    };
    let (body, proof) = prove_and_submit(&ctx, witness, None, recovery)?;
    submit(&ctx, &body, &proof)?;
    w.notes.push(stored);
    w.next_self += 1;
    w.save(path)
}

/// Rebuilds a wallet from the seed alone: scans every section, matches the
/// self-note tags locally (the node never learns which tags are ours),
/// opens their slots and rebuilds the notes. Stops after `GAP_LIMIT`
/// consecutive missing tags.
fn restore(path: &str, a: &Args) -> Result<(), String> {
    let seed: [u8; 32] = hex::decode(a.get("seed")?.trim_start_matches("0x"))
        .map_err(|e| e.to_string())?
        .try_into()
        .map_err(|_| "the seed is 32 bytes")?;
    let node = Node::connect(a.get("rpc")?)?;
    let chain = chain_id(&node)?;
    let sponge = Sponge::new(chain);
    let keys = WalletKeys::from_seed(&sponge, seed);
    let mut w = Wallet::new(seed);

    // Every applied transaction with its recovery slot and outputs.
    let status = node.call("shielded_getStatus", vec![])?;
    let executed = as_u64(&status["executedEpoch"])?;
    let mut by_tag = HashMap::new();
    let mut from = 0u64;
    while from <= executed {
        let sections = node.call(
            "shielded_getSections",
            vec![json!(format!("0x{:x}", from)), json!("0x3e8")],
        )?;
        for section in sections.as_array().ok_or("sections")? {
            for tx in section["transactions"].as_array().ok_or("transactions")? {
                if tx["applied"] == Value::Bool(true) {
                    by_tag.insert(unhex(&tx["recoveryTag"])?, tx.clone());
                }
            }
        }
        from = from.saturating_add(1000);
    }

    let rkey = recovery_key(&seed);
    let mut misses = 0;
    let mut i = 0u32;
    while misses < GAP_LIMIT && i < MAX_SELF_INDEX {
        let tag = recovery_tag(&rkey, i).to_vec();
        if let Some(tx) = by_tag.get(&tag) {
            misses = 0;
            let slot = RecoverySlot {
                tag: tag.clone().try_into().unwrap(),
                slot: unhex(&tx["recoverySlot"])?.try_into().map_err(|_| "slot length")?,
                mac: unhex(&tx["recoveryMac"])?.try_into().map_err(|_| "mac length")?,
            };
            let value = keys
                .open_recovery(i, &slot)
                .ok_or_else(|| format!("tag {i} matched but its slot does not open"))?;
            let note = keys.self_note(&sponge, i, value);
            let mut stored = StoredNote::from_note(&note, &sponge);
            stored.self_index = Some(i);
            w.notes.push(stored);
            w.next_self = i + 1;
        } else {
            misses += 1;
        }
        i += 1;
    }
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
fn deposit_credit(node: &Node, body_hash: &[u8]) -> Result<u64, String> {
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
    if bytes.len() != 32 || bytes[..24].iter().any(|b| *b != 0) {
        return Err(format!("unexpected deposit() result {out}"));
    }
    Ok(u64::from_be_bytes(bytes[24..].try_into().unwrap()))
}

fn send_native(
    node: &Node, secret: &mazzekey::Secret, to: Address, value: u64, data: Vec<u8>,
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
            value: value.into(),
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
