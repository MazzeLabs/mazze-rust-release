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
//! sender writes the recipient's note opening to a file.
//!
//! ```text
//! mazze-shielded-wallet init       --wallet W
//! mazze-shielded-wallet address    --wallet W
//! mazze-shielded-wallet sync       --wallet W --rpc URL
//! mazze-shielded-wallet balance    --wallet W
//! mazze-shielded-wallet shield-in  --wallet W --rpc URL --amount A --funder-key HEX
//! mazze-shielded-wallet send       --wallet W --rpc URL --to mzs1.. --amount A --opening-out FILE
//! mazze-shielded-wallet receive    --wallet W --opening FILE
//! mazze-shielded-wallet unshield   --wallet W --rpc URL --to 0xADDR [--espace] --amount A
//! ```

mod rpc;
mod store;

use std::{collections::HashMap, time::Duration};

use mazze_shielded_pq::{
    air::spend2x2::{SpendIn, Tx2Witness},
    field::{Digest, Sponge, DIGEST, F},
    note::{nullifier, Note},
    ps1::ProverRandomness,
    tree::{Tree, DEPTH},
    wallet::{build_transaction, fresh_fields, from_shielded_digest, WalletKeys},
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
            let node = Node::connect(a.get("rpc")?)?;
            let chain = chain_id(&node)?;
            println!("{}", address(&w, chain));
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
            let opening: StoredNote = serde_json::from_str(
                &std::fs::read_to_string(a.get("opening")?).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
            w.notes.push(opening);
            w.save(path)?;
            println!("imported; run sync to find it on chain");
            Ok(())
        }
        "sync" => {
            let mut w = Wallet::load(path)?;
            let node = Node::connect(a.get("rpc")?)?;
            sync(&mut w, &node)?;
            w.save(path)
        }
        "shield-in" => shield_in(path, &a),
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

/// `mzs1` + hex(d ‖ owner), the elements as 4-byte little-endian words.
fn address(w: &Wallet, chain: u32) -> String {
    let sponge = Sponge::new(chain);
    let keys = WalletKeys::from_seed(&sponge, w.seed);
    let d = keys.diversifier(0);
    let owner = keys.spend.owner(&sponge, &d);
    let mut bytes = Vec::new();
    for x in d.iter().chain(owner.iter()) {
        bytes.extend_from_slice(&x.as_canonical_u32().to_le_bytes());
    }
    format!("mzs1{}", hex::encode(bytes))
}

fn parse_address(s: &str) -> Result<([F; 3], Digest), String> {
    let bytes = hex::decode(s.strip_prefix("mzs1").ok_or("not an mzs1 address")?)
        .map_err(|e| e.to_string())?;
    if bytes.len() != 4 * (3 + DIGEST) {
        return Err("bad address length".into());
    }
    let elems: Vec<F> = bytes
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
        .map(|v| if v < F::ORDER_U32 { Ok(F::new(v)) } else { Err("non-canonical address") })
        .collect::<Result<_, _>>()?;
    Ok((std::array::from_fn(|i| elems[i]), std::array::from_fn(|i| elems[3 + i])))
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

fn random_recovery() -> RecoverySlot {
    let mut r = RecoverySlot { tag: [0; 32], slot: [0; 64], mac: [0; 16] };
    rand::rngs::OsRng.fill_bytes(&mut r.tag);
    rand::rngs::OsRng.fill_bytes(&mut r.slot);
    rand::rngs::OsRng.fill_bytes(&mut r.mac);
    r
}

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
) -> Result<(primitives::shielded::ShieldedTxBody, Vec<u8>), String> {
    eprintln!("proving…");
    let (body, proof) =
        build_transaction(ctx.chain, &mut witness, dest, random_recovery(), prover_randomness)?;
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

fn self_note(ctx: &Context, value: u64) -> Note {
    let d = ctx.keys.diversifier(0);
    new_note(d, ctx.keys.spend.owner(&ctx.sponge, &d), value)
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
    let mine = self_note(&ctx, amount - ctx.fee);
    let witness = Tx2Witness {
        inputs: [dummy_input(&ctx.sponge), dummy_input(&ctx.sponge)],
        outputs: shuffle_outputs(mine.clone(), dummy_output()),
        v_in: amount,
        v_out: 0,
        fee: ctx.fee,
        sighash: [0; 32],
        anchor: tree(&w).root(),
        out_limb_override: [None, None],
    };
    let (body, proof) = prove_and_submit(&ctx, witness, None)?;

    // The deposit: a signed native call to the bridge with the amount.
    let secret = mazzekey::Secret::from_slice(
        &hex::decode(a.get("funder-key")?.trim_start_matches("0x")).map_err(|e| e.to_string())?,
    )
    .ok_or("--funder-key must be a 32-byte secret")?;
    let mut data = keccak_hash::keccak(b"shieldIn(bytes32)").0[..4].to_vec();
    data.extend_from_slice(body.hash().as_bytes());
    let receipt = send_native(&ctx.node, &secret, Address::from(SHIELDED_BRIDGE_ADDRESS), amount, data)?;
    if receipt["outcomeStatus"] != json!("0x0") {
        return Err(format!("the deposit failed: {receipt}"));
    }
    println!("deposit executed");
    submit(&ctx, &body, &proof)?;
    w.notes.push(StoredNote::from_note(&mine, &ctx.sponge));
    w.save(path)
}

fn send(path: &str, a: &Args) -> Result<(), String> {
    let mut w = Wallet::load(path)?;
    let ctx = context(&mut w, a)?;
    let amount = a.amount()?;
    let (d, owner) = parse_address(a.get("to")?)?;
    let inputs = select_inputs(&w, amount + ctx.fee)?;
    let total: u64 = inputs.iter().map(|&i| w.notes[i].value).sum();
    let t = tree(&w);
    let input = |k: usize| inputs.get(k).map(|&i| spend_input(&ctx, &w, &t, i)).unwrap_or_else(|| dummy_input(&ctx.sponge));
    let theirs = new_note(d, owner, amount);
    let change = self_note(&ctx, total - amount - ctx.fee);
    let witness = Tx2Witness {
        inputs: [input(0), input(1)],
        outputs: shuffle_outputs(theirs.clone(), change.clone()),
        v_in: 0,
        v_out: 0,
        fee: ctx.fee,
        sighash: [0; 32],
        anchor: t.root(),
        out_limb_override: [None, None],
    };
    let (body, proof) = prove_and_submit(&ctx, witness, None)?;
    submit(&ctx, &body, &proof)?;
    let opening = StoredNote::from_note(&theirs, &ctx.sponge);
    std::fs::write(a.get("opening-out")?, serde_json::to_string_pretty(&opening).unwrap())
        .map_err(|e| e.to_string())?;
    println!("recipient's note opening written to {}", a.get("opening-out")?);
    w.notes.push(StoredNote::from_note(&change, &ctx.sponge));
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
    let change = self_note(&ctx, total - amount - ctx.fee);
    let witness = Tx2Witness {
        inputs: [input(0), input(1)],
        outputs: shuffle_outputs(change.clone(), dummy_output()),
        v_in: 0,
        v_out: amount,
        fee: ctx.fee,
        sighash: [0; 32],
        anchor: t.root(),
        out_limb_override: [None, None],
    };
    let (body, proof) = prove_and_submit(&ctx, witness, Some(ShieldedDest { space, address: to }))?;
    submit(&ctx, &body, &proof)?;
    w.notes.push(StoredNote::from_note(&change, &ctx.sponge));
    w.save(path)
}

// ---- the native deposit ------------------------------------------------------

fn send_native(
    node: &Node, secret: &mazzekey::Secret, to: Address, value: u64, data: Vec<u8>,
) -> Result<Value, String> {
    let status = node.call("mazze_getStatus", vec![])?;
    let chain_id = as_u64(&status["chainId"])? as u32;
    let network = match as_u64(&status["networkId"])? {
        1 => mazze_addr::Network::Test,
        1990 => mazze_addr::Network::Main,
        n => mazze_addr::Network::Id(n),
    };
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
