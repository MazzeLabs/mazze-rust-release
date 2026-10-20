//! The wallet file: the seed, the rebuilt tree, and the note openings.

use mazze_shielded_pq::{
    field::{Digest, Sponge, DIGEST, F},
    note::{Note, HM_L},
};
use p3_field::PrimeField32;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct Wallet {
    #[serde(with = "hex32")]
    pub seed: [u8; 32],
    /// The next epoch to fetch.
    pub synced_epoch: u64,
    /// Every leaf of the commitment tree, in position order.
    pub leaves: Vec<[u32; DIGEST]>,
    pub notes: Vec<StoredNote>,
}

/// A note opening: what the owner needs to spend it. As written to a file
/// for the recipient, it is the out-of-band delivery of a payment.
#[derive(Serialize, Deserialize, Clone)]
pub struct StoredNote {
    pub d: [u32; 3],
    pub value: u64,
    pub psi: Vec<u32>,
    pub y: Vec<u32>,
    pub cm: [u32; DIGEST],
    #[serde(default)]
    pub position: Option<u64>,
    #[serde(default)]
    pub spent: bool,
}

fn elems<const N: usize>(xs: &[u32]) -> [F; N] { std::array::from_fn(|i| F::new(xs[i])) }

impl StoredNote {
    pub fn from_note(note: &Note, sponge: &Sponge) -> Self {
        StoredNote {
            d: note.d.map(|x| x.as_canonical_u32()),
            value: note.value,
            psi: note.psi.iter().map(|x| x.as_canonical_u32()).collect(),
            y: note.y.iter().map(|x| x.as_canonical_u32()).collect(),
            cm: note.cm(sponge).map(|x| x.as_canonical_u32()),
            position: None,
            spent: false,
        }
    }

    pub fn d(&self) -> [F; 3] { elems(&self.d) }

    pub fn psi(&self) -> Digest { elems::<DIGEST>(&self.psi) }

    pub fn y(&self) -> [F; HM_L] { elems::<HM_L>(&self.y) }
}

impl Wallet {
    pub fn new(seed: [u8; 32]) -> Self {
        Wallet { seed, synced_epoch: 0, leaves: Vec::new(), notes: Vec::new() }
    }

    pub fn load(path: &str) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
        serde_json::from_str(&text).map_err(|e| format!("{path}: {e}"))
    }

    /// Written atomically: a crash never leaves a half-written wallet.
    pub fn save(&self, path: &str) -> Result<(), String> {
        let tmp = format!("{path}.tmp");
        std::fs::write(&tmp, serde_json::to_string(self).unwrap()).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, path).map_err(|e| e.to_string())
    }
}

mod hex32 {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(v))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        let s = String::deserialize(d)?;
        let v = hex::decode(s).map_err(serde::de::Error::custom)?;
        v.try_into().map_err(|_| serde::de::Error::custom("seed must be 32 bytes"))
    }
}
