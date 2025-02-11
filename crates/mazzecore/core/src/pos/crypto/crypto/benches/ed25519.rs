// Copyright (c) The Diem Core Contributors
// SPDX-License-Identifier: Apache-2.0

// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

#[macro_use]
extern crate criterion;

use criterion::Criterion;

use diem_crypto_derive::{BCSCryptoHash, CryptoHasher};
use rand::{prelude::ThreadRng, thread_rng};
use serde::{Deserialize, Serialize};

#[derive(Debug, CryptoHasher, BCSCryptoHash, Serialize, Deserialize)]
pub struct TestDiemCrypto(pub String);

use diem_crypto::{
    ed25519::{Ed25519PrivateKey, Ed25519PublicKey, Ed25519Signature},
    traits::{Signature, SigningKey, Uniform},
};

fn verify(c: &mut Criterion) {
    let mut csprng: ThreadRng = thread_rng();
    let priv_key = Ed25519PrivateKey::generate(&mut csprng);
    let pub_key: Ed25519PublicKey = (&priv_key).into();
    let msg = TestDiemCrypto("".to_string());
    let sig: Ed25519Signature = priv_key.sign(&msg);

    c.bench_function("Ed25519 signature verification", move |b| {
        b.iter(|| sig.verify(&msg, &pub_key))
    });
}

criterion_group!(ed25519_benches, verify);
criterion_main!(ed25519_benches);
