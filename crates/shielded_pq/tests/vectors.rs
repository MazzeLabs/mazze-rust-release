//! Cross-implementation vectors: these values were produced by the spike
//! prototype (spikes/shielded-notes, tests/vectors.rs) with chain id 19802.
//! A wallet built on the prototype and this crate must agree bit for bit.

use mazze_shielded_pq::{
    field::{Digest, Sponge, Xof, F},
    keys::SpendKeys,
    note::{hm_matrix, nullifier, Note},
    tree::Tree,
};
use p3_field::{PrimeCharacteristicRing, PrimeField32};

const SPIKE_CHAIN_ID: u32 = 19802;

fn d(seed: u32) -> Digest { std::array::from_fn(|i| F::new(seed * 1000 + i as u32)) }
fn u(xs: &[F]) -> Vec<u32> { xs.iter().map(|x| x.as_canonical_u32()).collect() }

#[test]
fn matches_the_prototype() {
    let s = Sponge::new(SPIKE_CHAIN_ID);
    let a = hm_matrix();
    assert_eq!(u(&a[0][..4]), [847630550, 1886882492, 1587263368, 325629745]);
    assert_eq!(u(&a[27][60..]), [312923657, 1259890470, 1088760232, 2123261210]);
    assert_eq!(
        u(&Xof::new("mazze/test", &[b"abc"]).fields::<4>()),
        [406198697, 1288525408, 1551956911, 1055592425]
    );
    let keys = SpendKeys::from_nsk(&s, d(1));
    assert_eq!(u(&keys.ak), [693032177, 1115816934, 423813968, 555773477, 1087401718, 75711439, 1174130807, 1517990415, 1717525154, 1128425286, 1352122772, 567870619]);
    assert_eq!(u(&keys.nk), [2111190497, 786465763, 544742707, 907680800, 671831137, 1084742372, 711560971, 1612779501, 1119471643, 1290670185, 90393732, 1280479129]);
    let div = [F::new(7), F::new(8), F::new(9)];
    let owner = keys.owner(&s, &div);
    assert_eq!(u(&owner), [258809523, 1355483440, 1750927162, 1680536426, 812933847, 100373578, 2067895428, 1587309277, 130037581, 1286046025, 698424307, 671190839]);
    let y: [F; 64] = std::array::from_fn(|i| F::new(5000 + i as u32));
    let note = Note { d: div, owner, value: 123_456_789, psi: d(2), y };
    let cm = note.cm(&s);
    assert_eq!(u(&cm), [600978626, 1770875173, 1652814060, 1302600931, 428813605, 829156714, 1234190805, 131738991, 1640867767, 1749989795, 2129008388, 476067186]);
    assert_eq!(u(&nullifier(&s, &keys.nk, &note.psi, 77)), [1994153425, 662633234, 991746860, 1087717865, 441990238, 1772148886, 1657189122, 438366766, 1630338512, 1401547387, 1738232925, 1449348500]);
    let mut t = Tree::default();
    t.append(cm);
    t.append(d(3));
    assert_eq!(u(&t.root()), [1769544160, 2099745445, 10339067, 1495349222, 1902603979, 588337888, 242467934, 1877320616, 368435660, 1072950964, 160625908, 1048519974]);
    assert_eq!(u(&Tree::default().root()), [1548222333, 2052968329, 1352039291, 202825213, 1839802404, 1007136071, 1121785814, 28922775, 1247668070, 1465183506, 157122081, 849410758]);
    let _ = F::ZERO;
}

#[test]
fn chain_id_changes_every_commitment() {
    let y: [F; 64] = std::array::from_fn(|i| F::new(5000 + i as u32));
    let note = Note { d: [F::ONE; 3], owner: d(4), value: 1, psi: d(2), y };
    assert_ne!(note.cm(&Sponge::new(1)), note.cm(&Sponge::new(2)));
}
