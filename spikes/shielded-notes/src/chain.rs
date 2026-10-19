//! A minimal shielded ledger: the 2×2 transaction, the spend relation and the
//! state transition (spec §6–§8).
//!
//! The STARK proof is replaced by `Proof::Simulated`, which carries the witness
//! and is checked by `check_relation`, the *same statement* the AIR will
//! enforce (C1–C10). The ledger never reads the witness except through that
//! function. So "Bob can build a witness that passes `check_relation`" is
//! exactly "Bob can produce a valid proof".

use std::collections::{HashMap, HashSet};

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::ChaCha20Poly1305;

use crate::keys::SpendKeys;
use crate::note::*;
use crate::primitives::*;
use crate::tree::*;

/// Standard fee: every transaction has the same shape and weight, so it pays
/// the same (spec §6.1, fee fingerprinting).
pub const FEE: u64 = 1;

/// Fixed-size recovery slot owned by the transaction's sender (spec §9.5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recovery {
    pub tag: [u8; 32],
    pub ct: [u8; 64],
    pub mac: [u8; 16],
}

impl Recovery {
    pub fn seal(rkey: &[u8; 32], i: u32, value: u64) -> Self {
        let tag = recovery_tag(rkey, i);
        let key: [u8; 32] = Xof::new("mazze/rec/slot", &[rkey, &i.to_le_bytes()]).bytes();
        let mut pt = [0u8; 64];
        pt[..8].copy_from_slice(&value.to_le_bytes());
        // Single-use key per index, so a fixed nonce is safe (SIV-like determinism).
        let out = ChaCha20Poly1305::new(&key.into()).encrypt(&[0u8; 12].into(), &pt[..]).unwrap();
        Recovery { tag, ct: out[..64].try_into().unwrap(), mac: out[64..].try_into().unwrap() }
    }

    pub fn open(&self, rkey: &[u8; 32], i: u32) -> Option<u64> {
        let key: [u8; 32] = Xof::new("mazze/rec/slot", &[rkey, &i.to_le_bytes()]).bytes();
        let mut ct = self.ct.to_vec();
        ct.extend_from_slice(&self.mac);
        let pt = ChaCha20Poly1305::new(&key.into()).decrypt(&[0u8; 12].into(), &ct[..]).ok()?;
        Some(u64::from_le_bytes(pt[..8].try_into().unwrap()))
    }

    /// Filler for a sender that doesn't use its slot: indistinguishable bytes.
    pub fn random() -> Self {
        Recovery { tag: rand::random(), ct: std::array::from_fn(|_| rand::random()), mac: rand::random() }
    }

    fn bytes(&self) -> Vec<u8> {
        [&self.tag[..], &self.ct[..], &self.mac[..]].concat()
    }
}

pub fn recovery_tag(rkey: &[u8; 32], i: u32) -> [u8; 32] {
    Xof::new("mazze/rec/tag", &[rkey, &i.to_le_bytes()]).bytes()
}

#[derive(Clone, Debug)]
pub struct InputWitness {
    pub nsk: Digest,
    /// Note fields except `owner`, which the relation recomputes from `nsk`.
    pub note: NoteOpening,
    pub pos: u64,
    pub path: [Digest; DEPTH],
}

#[derive(Clone, Debug)]
pub struct Witness {
    pub inputs: [InputWitness; 2],
    pub outputs: [Note; 2],
}

#[derive(Clone, Debug)]
pub enum Proof {
    /// Stand-in for the STARK: the witness, bound to the sighash.
    Simulated { sighash: [u8; 32], witness: Witness },
}

/// PS-1 at launch: KoalaBear^8 + FRI + Keccak commitments/Fiat–Shamir (spec §6.4).
pub const PS_1: u8 = 1;

#[derive(Clone, Debug)]
pub struct ShieldedTx {
    /// How the statement is proven. Notes and state never depend on it.
    pub proof_system_id: u8,
    pub anchor: Digest,
    pub nf: [Digest; 2],
    pub cm_out: [Digest; 2],
    pub recovery: Recovery,
    pub v_public_in: u64,
    pub v_public_out: u64,
    pub dest: Option<[u8; 20]>,
    pub fee: u64,
    pub proof: Proof,
}

impl ShieldedTx {
    /// Every public field goes into the sighash, and the sighash is a public
    /// input of the proof (spec §6.3, Frozen-Heart rule).
    pub fn sighash(&self) -> [u8; 32] {
        let dest = self.dest.map(|d| d.to_vec()).unwrap_or_default();
        Xof::new(
            "mazze/sighash/v1",
            &[
                &CHAIN_ID.to_le_bytes(),
                &[self.proof_system_id],
                &to_bytes(&self.anchor),
                &to_bytes(&self.nf.concat()),
                &to_bytes(&self.cm_out.concat()),
                &self.recovery.bytes(),
                &self.v_public_in.to_le_bytes(),
                &self.v_public_out.to_le_bytes(),
                &dest,
                &self.fee.to_le_bytes(),
            ],
        )
        .bytes()
    }

    /// Bytes this transaction puts in a block, excluding the proof (which goes
    /// to aggregation).
    pub fn body_len(&self) -> usize {
        4 * DIGEST * 5 + 112 + 8 * 3 + 20 + 2
    }
}

/// The spend statement (C1–C10). Returns the reason on failure.
pub fn check_relation(tx: &ShieldedTx, w: &Witness) -> Result<(), String> {
    let mut sum_in: u128 = tx.v_public_in as u128;
    for (i, inp) in w.inputs.iter().enumerate() {
        // C1, C2: ak and nk both come from nsk, and owner binds nk.
        let keys = SpendKeys::from_nsk(inp.nsk);
        let owner = keys.owner(&inp.note.d);
        // C3: the input commitment, with the owner derived from nsk.
        let cm_in = inp.note.clone().into_note(owner).cm();
        // C4: membership unless value == 0 (the gate is a function of the value).
        if inp.note.value != 0 && root_from_path(&cm_in, inp.pos, &inp.path) != tx.anchor {
            return Err(format!("input {i}: not in the tree under the anchor (or wrong owner key)"));
        }
        // C5: the unique nullifier of that leaf.
        if nullifier(&keys.nk, &inp.note.psi, inp.pos) != tx.nf[i] {
            return Err(format!("input {i}: nullifier mismatch"));
        }
        sum_in += inp.note.value as u128;
    }
    let mut sum_out: u128 = tx.v_public_out as u128 + tx.fee as u128;
    for (k, out) in w.outputs.iter().enumerate() {
        // C7: well-formed output commitments.
        if out.cm() != tx.cm_out[k] {
            return Err(format!("output {k}: commitment mismatch"));
        }
        sum_out += out.value as u128;
    }
    // C8 is implicit (u64 values, 16-bit limbs); C9 is checked over integers, never mod p.
    if sum_in != sum_out {
        return Err(format!("balance: in {sum_in} != out {sum_out}"));
    }
    Ok(())
}

#[derive(Default)]
pub struct Ledger {
    pub tree: Tree,
    pub nullifiers: HashSet<Vec<u8>>,
    pub roots: HashSet<Vec<u8>>,
    pub txs: Vec<ShieldedTx>,
    /// Public recovery-tag index (spec §9.5): tag → transaction index.
    pub tags: HashMap<[u8; 32], usize>,
    pub backing: u64,
    pub burnt: u64,
    /// Consensus `allowed_proof_systems` / `default_proof_system`, changed only by hardfork.
    pub allowed_proof_systems: Vec<u8>,
    pub default_proof_system: u8,
}

impl Ledger {
    pub fn new() -> Self {
        let mut l = Ledger::default();
        l.roots.insert(to_bytes(&l.tree.root()));
        l.allowed_proof_systems = vec![PS_1];
        l.default_proof_system = PS_1;
        l
    }

    pub fn is_spent(&self, nf: &Digest) -> bool {
        self.nullifiers.contains(&to_bytes(nf))
    }

    /// A hardfork at a transition height: a new allowed set and default.
    pub fn hardfork_proof_systems(&mut self, allowed: &[u8], default: u8) {
        assert!(allowed.contains(&default));
        self.allowed_proof_systems = allowed.to_vec();
        self.default_proof_system = default;
    }

    /// The node's pipeline (spec §8.2), fail-closed at each step.
    pub fn apply(&mut self, tx: ShieldedTx) -> Result<[u64; 2], String> {
        if !self.allowed_proof_systems.contains(&tx.proof_system_id) {
            return Err(format!("proof system {} not allowed at this height", tx.proof_system_id));
        }
        if tx.dest.is_some() != (tx.v_public_out > 0) {
            return Err("dest present iff v_public_out > 0".into());
        }
        if tx.fee != FEE {
            return Err("non-standard fee".into());
        }
        if !self.roots.contains(&to_bytes(&tx.anchor)) {
            return Err("unknown anchor".into());
        }
        if tx.nf[0] == tx.nf[1] || tx.nf.iter().any(|nf| self.is_spent(nf)) {
            return Err("double spend".into());
        }
        let Proof::Simulated { sighash, witness } = &tx.proof;
        if *sighash != tx.sighash() {
            return Err("proof not bound to this transaction".into());
        }
        check_relation(&tx, witness)?;
        let backing = (self.backing as u128 + tx.v_public_in as u128)
            .checked_sub(tx.v_public_out as u128 + tx.fee as u128)
            .ok_or("turnstile: backing would go negative")?;
        // Commit.
        for nf in &tx.nf {
            self.nullifiers.insert(to_bytes(nf));
        }
        let pos = [self.tree.append(tx.cm_out[0]), self.tree.append(tx.cm_out[1])];
        self.roots.insert(to_bytes(&self.tree.root()));
        self.tags.insert(tx.recovery.tag, self.txs.len());
        self.backing = backing as u64;
        self.burnt += tx.fee;
        self.txs.push(tx);
        Ok(pos)
    }
}
