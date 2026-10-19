//! Prints reference vectors that the node crate pins (cross-implementation check).
use p3_field::{PrimeCharacteristicRing, PrimeField32};
use shielded_notes_spike::keys::SpendKeys;
use shielded_notes_spike::note::{hm_matrix, nullifier, Note};
use shielded_notes_spike::primitives::*;
use shielded_notes_spike::tree::Tree;

fn d(seed: u32) -> Digest { std::array::from_fn(|i| F::new(seed * 1000 + i as u32)) }
fn show(name: &str, xs: &[F]) {
    println!("VEC {name} {:?}", xs.iter().map(|x| x.as_canonical_u32()).collect::<Vec<_>>());
}

#[test]
#[ignore]
fn reference_vectors() {
    let a = hm_matrix();
    show("A00", &a[0][..4]);
    show("A_last", &a[27][60..]);
    show("xof", &Xof::new("mazze/test", &[b"abc"]).fields::<4>());
    let keys = SpendKeys::from_nsk(d(1));
    show("ak", &keys.ak);
    show("nk", &keys.nk);
    let div = [F::new(7), F::new(8), F::new(9)];
    show("owner", &keys.owner(&div));
    let y: [F; 64] = std::array::from_fn(|i| F::new(5000 + i as u32));
    let note = Note { d: div, owner: keys.owner(&div), value: 123_456_789, psi: d(2), y };
    show("cm", &note.cm());
    show("nf", &nullifier(&keys.nk, &note.psi, 77));
    let mut t = Tree::default();
    t.append(note.cm());
    t.append(d(3));
    show("root", &t.root());
    show("empty_root", &Tree::default().root());
    println!("VEC chain_id {}", CHAIN_ID);
}
