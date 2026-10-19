//! A wallet that does everything spec §9 asks of one: shield-in, pay (in
//! recipient-blind or no-trace mode), receive an opening, claim automatically
//! into a seed-recoverable note, restore from the seed alone, reconstruct its
//! payment history without recipients, and resend.

use p3_field::PrimeCharacteristicRing;

use crate::chain::*;
use crate::keys::*;
use crate::note::*;
use crate::primitives::*;
use crate::seal;
use crate::tree::DEPTH;

/// Diversifier index used for the wallet's own (self) notes.
pub const INTERNAL: u32 = u32::MAX;
/// Addresses a wallet checks when receiving.
pub const ADDRESS_GAP: u32 = 4;
/// Consecutive missing recovery tags before restore stops (spec §9.5).
pub const GAP_LIMIT: u32 = 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendMode {
    /// The note randomness comes from `out_key` and an index, with no
    /// recipient data. The sender can resend later after re-entering the
    /// recipient's address.
    RecipientBlind,
    /// Fresh OS randomness. Nothing about the payment is recoverable from the
    /// seed; use it with deliver-then-publish.
    NoTrace,
}

#[derive(Clone, Debug)]
pub struct Owned {
    pub note: Note,
    pub pos: u64,
}

/// One outgoing payment reconstructed from seed and chain. There is no
/// recipient field, by design.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SentEntry {
    pub tx_index: usize,
    pub amount: u64,
    pub out_index: u32,
}

pub struct Wallet {
    pub seed: Seed,
    pub keys: SpendKeys,
    pub owned: Vec<Owned>,
    pub pending: Vec<Owned>,
    next_self: u32,
    next_out: u32,
}

impl Wallet {
    pub fn new(seed: Seed) -> Self {
        let keys = seed.spend_keys();
        Wallet { seed, keys, owned: vec![], pending: vec![], next_self: 0, next_out: 0 }
    }

    pub fn address(&self, j: u32) -> (Address, Bundle) {
        self.seed.address(j)
    }

    /// Self-note `i`. All parameters come from the seed, and the value is
    /// what the recovery slot stores.
    pub fn self_note(&self, i: u32, value: u64) -> Note {
        let mut x = Xof::new("mazze/self-note", &[&self.seed.rkey(), &i.to_le_bytes()]);
        let d = self.seed.diversifier(INTERNAL);
        Note { d, owner: self.keys.owner(&d), value, psi: x.fields(), y: x.fields() }
    }

    fn out_note(&self, k: u32, to: &Address, amount: u64) -> Note {
        let mut x = Xof::new("mazze/out-note", &[&self.seed.out_key(), &k.to_le_bytes()]);
        Note { d: to.d, owner: to.owner, value: amount, psi: x.fields(), y: x.fields() }
    }

    pub fn spendable(&self, l: &Ledger) -> Vec<Owned> {
        self.owned
            .iter()
            .filter(|o| !l.is_spent(&nullifier(&self.keys.nk, &o.note.psi, o.pos)))
            .cloned()
            .collect()
    }

    pub fn balance(&self, l: &Ledger) -> u64 {
        self.spendable(l).iter().map(|o| o.note.value).sum()
    }

    fn dummy_input() -> InputWitness {
        InputWitness {
            nsk: random_fields(),
            note: NoteOpening { d: random_fields(), value: 0, psi: random_fields(), y: random_fields() },
            pos: 0,
            path: [[F::ZERO; DIGEST]; DEPTH],
        }
    }

    fn dummy_output() -> Note {
        Note { d: random_fields(), owner: random_fields(), value: 0, psi: random_fields(), y: random_fields() }
    }

    fn build(
        &self,
        l: &Ledger,
        inputs: &[Owned],
        outputs: [Note; 2],
        v_in: u64,
        v_out: u64,
        dest: Option<[u8; 20]>,
        recovery: Recovery,
    ) -> ShieldedTx {
        let mut iw: Vec<InputWitness> = inputs
            .iter()
            .map(|o| InputWitness {
                nsk: self.keys.nsk,
                note: NoteOpening::of(&o.note),
                pos: o.pos,
                path: l.tree.path(o.pos),
            })
            .collect();
        while iw.len() < 2 {
            iw.push(Self::dummy_input());
        }
        let nf = std::array::from_fn(|i| {
            let k = SpendKeys::from_nsk(iw[i].nsk);
            nullifier(&k.nk, &iw[i].note.psi, iw[i].pos)
        });
        let witness = Witness { inputs: [iw[0].clone(), iw[1].clone()], outputs: outputs.clone() };
        let mut tx = ShieldedTx {
            proof_system_id: l.default_proof_system,
            anchor: l.tree.root(),
            nf,
            cm_out: [outputs[0].cm(), outputs[1].cm()],
            recovery,
            v_public_in: v_in,
            v_public_out: v_out,
            dest,
            fee: FEE,
            proof: Proof::Simulated { sighash: [0; 32], witness },
        };
        let sh = tx.sighash();
        let Proof::Simulated { sighash, .. } = &mut tx.proof;
        *sighash = sh;
        tx
    }

    fn take_self_index(&mut self) -> u32 {
        let i = self.next_self;
        self.next_self += 1;
        i
    }

    /// Shield-in: transparent `amount` → a self-note (no delivery needed).
    pub fn shield_in(&mut self, l: &mut Ledger, amount: u64) -> Result<(), String> {
        let i = self.take_self_index();
        let note = self.self_note(i, amount - FEE);
        let rec = Recovery::seal(&self.seed.rkey(), i, note.value);
        let tx = self.build(l, &[], [note.clone(), Self::dummy_output()], amount, 0, None, rec);
        let pos = l.apply(tx)?;
        self.owned.push(Owned { note, pos: pos[0] });
        Ok(())
    }

    fn select(&self, l: &Ledger, need: u64) -> Result<Vec<Owned>, String> {
        let mut notes = self.spendable(l);
        notes.sort_by(|a, b| b.note.value.cmp(&a.note.value));
        let pick: Vec<Owned> = notes.into_iter().take(2).collect();
        if pick.iter().map(|o| o.note.value).sum::<u64>() < need {
            return Err("insufficient funds in at most 2 notes (merge first)".into());
        }
        Ok(pick)
    }

    /// Pay `amount` to an address. Returns the opening to deliver off-chain.
    pub fn pay(&mut self, l: &mut Ledger, to: &Address, amount: u64, mode: SendMode) -> Result<NoteOpening, String> {
        let inputs = self.select(l, amount + FEE)?;
        let change = inputs.iter().map(|o| o.note.value).sum::<u64>() - amount - FEE;
        // Every payment consumes an out index, so indices in the reconstructed
        // history line up with derivation even when no-trace payments are mixed in.
        let k = self.next_out;
        self.next_out += 1;
        let recipient_note = match mode {
            SendMode::RecipientBlind => self.out_note(k, to, amount),
            SendMode::NoTrace => Note {
                d: to.d,
                owner: to.owner,
                value: amount,
                psi: random_fields(),
                y: random_fields(),
            },
        };
        let i = self.take_self_index();
        let change_note = self.self_note(i, change);
        let rec = Recovery::seal(&self.seed.rkey(), i, change);
        let tx = self.build(l, &inputs, [recipient_note.clone(), change_note.clone()], 0, 0, None, rec);
        let pos = l.apply(tx)?;
        self.owned.push(Owned { note: change_note, pos: pos[1] });
        Ok(NoteOpening::of(&recipient_note))
    }

    /// Shield-out to a transparent destination.
    pub fn shield_out(&mut self, l: &mut Ledger, dest: [u8; 20], amount: u64) -> Result<(), String> {
        let inputs = self.select(l, amount + FEE)?;
        let change = inputs.iter().map(|o| o.note.value).sum::<u64>() - amount - FEE;
        let i = self.take_self_index();
        let change_note = self.self_note(i, change);
        let rec = Recovery::seal(&self.seed.rkey(), i, change);
        let tx = self.build(l, &inputs, [change_note.clone(), Self::dummy_output()], 0, amount, Some(dest), rec);
        let pos = l.apply(tx)?;
        self.owned.push(Owned { note: change_note, pos: pos[0] });
        Ok(())
    }

    /// Receive an opening. Bob verifies everything himself: the owner must be
    /// one of his addresses, and the commitment must be in the tree.
    pub fn receive(&mut self, l: &Ledger, opening: NoteOpening) -> Result<u64, String> {
        let owner = (0..ADDRESS_GAP)
            .map(|j| self.seed.diversifier(j))
            .find(|d| *d == opening.d)
            .map(|d| self.keys.owner(&d))
            .ok_or("opening is not for any of my addresses")?;
        let note = opening.into_note(owner);
        let pos = l.tree.find(&note.cm()).ok_or("commitment not on chain (forged or not final yet)")?;
        if l.is_spent(&nullifier(&self.keys.nk, &note.psi, pos)) {
            return Err("already spent".into());
        }
        let value = note.value;
        self.pending.push(Owned { note, pos });
        Ok(value)
    }

    /// Open a sealed blob addressed to one of my addresses.
    pub fn open_sealed(&self, blob: &[u8]) -> Option<NoteOpening> {
        (0..ADDRESS_GAP).find_map(|j| {
            let keys = self.seed.delivery_keys(j);
            seal::open(&keys, &keys.bundle().hash(), blob)
        })
    }

    /// The mandatory automatic claim: move every pending note into a self-note
    /// that the seed alone can recover. It also **consolidates**: the second
    /// input is the wallet's largest spendable note, not a dummy, so the wallet
    /// stays at one main note and almost any payment fits one 2×2 transaction.
    pub fn claim_all(&mut self, l: &mut Ledger) -> Result<usize, String> {
        let pending = std::mem::take(&mut self.pending);
        for p in &pending {
            let main = self
                .spendable(l)
                .into_iter()
                .max_by_key(|o| o.note.value);
            let mut inputs = vec![p.clone()];
            if let Some(m) = main {
                inputs.push(m);
            }
            let total: u64 = inputs.iter().map(|o| o.note.value).sum();
            let i = self.take_self_index();
            let note = self.self_note(i, total - FEE);
            let rec = Recovery::seal(&self.seed.rkey(), i, note.value);
            let tx = self.build(l, &inputs, [note.clone(), Self::dummy_output()], 0, 0, None, rec);
            let pos = l.apply(tx)?;
            self.owned.push(Owned { note, pos: pos[0] });
        }
        Ok(pending.len())
    }

    /// Restore from the seed alone. This is a forward scan of recovery tags
    /// with a gap limit, matched locally against the public tag index.
    pub fn restore(seed: Seed, l: &Ledger) -> Self {
        let mut w = Wallet::new(seed);
        let rkey = w.seed.rkey();
        let (mut i, mut misses) = (0u32, 0u32);
        while misses < GAP_LIMIT {
            if let Some(&txi) = l.tags.get(&recovery_tag(&rkey, i)) {
                misses = 0;
                let value = l.txs[txi].recovery.open(&rkey, i).expect("my tag, my key");
                let note = w.self_note(i, value);
                let pos = l.tree.find(&note.cm()).expect("self-note on chain");
                w.owned.push(Owned { note, pos });
                w.next_self = i + 1;
            } else {
                misses += 1;
            }
            i += 1;
        }
        w.next_out = w.history(l).iter().map(|e| e.out_index + 1).max().unwrap_or(0);
        w
    }

    /// Outgoing payments reconstructed from seed and chain, as (tx, amount),
    /// with no recipient.
    ///
    /// A payment is a transaction that spends my notes, carries my change in
    /// output 1, and has a non-self output 0. amount = inputs − change − fee.
    pub fn history(&self, l: &Ledger) -> Vec<SentEntry> {
        let mine: Vec<(Digest, u64)> = self
            .owned
            .iter()
            .map(|o| (nullifier(&self.keys.nk, &o.note.psi, o.pos), o.note.value))
            .collect();
        let self_cms: Vec<Digest> = self.owned.iter().map(|o| o.note.cm()).collect();
        let mut out = vec![];
        let mut k = 0u32;
        for (txi, tx) in l.txs.iter().enumerate() {
            let spent_in: u64 = tx.nf.iter().filter_map(|nf| mine.iter().find(|m| m.0 == *nf)).map(|m| m.1).sum();
            if spent_in == 0 || tx.v_public_out > 0 || self_cms.contains(&tx.cm_out[0]) {
                continue; // not mine, a shield-out, or a claim/merge to myself
            }
            let change = self.owned.iter().find(|o| o.note.cm() == tx.cm_out[1]).map(|o| o.note.value);
            if let Some(change) = change {
                out.push(SentEntry { tx_index: txi, amount: spent_in - change - tx.fee, out_index: k });
                k += 1;
            }
        }
        out
    }

    /// Resend a recipient-blind payment. The user re-enters who it was for;
    /// the wallet checks the guess against the on-chain commitment.
    pub fn resend(&self, l: &Ledger, entry: &SentEntry, to: &Address) -> Result<NoteOpening, String> {
        let note = self.out_note(entry.out_index, to, entry.amount);
        if note.cm() != l.txs[entry.tx_index].cm_out[0] {
            return Err("that address is not the recipient of this payment".into());
        }
        Ok(NoteOpening::of(&note))
    }
}
