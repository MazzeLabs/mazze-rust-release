//! Spend keys (spec §3). `nsk` is the only spend secret; `ak` and `nk` are
//! both derived from it (Orchard lesson, Z-1), and `nk` is committed into
//! every note's owner. The node holds no shielded keys (Z-7): this module is
//! here for the prover and for tests.

use crate::field::{dom, Digest, Sponge, DIGEST, F};

#[derive(Clone)]
pub struct SpendKeys {
    pub nsk: Digest,
    pub ak: Digest,
    pub nk: Digest,
}

impl SpendKeys {
    pub fn from_nsk(sponge: &Sponge, nsk: Digest) -> Self {
        let ak = sponge.hash(dom::AK, &nsk);
        let nk = sponge.hash(dom::NK, &nsk);
        SpendKeys { nsk, ak, nk }
    }

    /// `owner_d = H_OWN(ak ‖ nk ‖ d)`.
    pub fn owner(&self, sponge: &Sponge, d: &[F; 3]) -> Digest {
        let mut input = Vec::with_capacity(2 * DIGEST + 3);
        input.extend_from_slice(&self.ak);
        input.extend_from_slice(&self.nk);
        input.extend_from_slice(d);
        sponge.hash(dom::OWN, &input)
    }
}
