#![allow(dead_code)]
// Copyright 2015-2019 Parity Technologies (UK) Ltd.
// This file is part of Parity Ethereum.

// Parity Ethereum is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// Parity Ethereum is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with Parity Ethereum.  If not, see <http://www.gnu.org/licenses/>.

// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

//! A map of subscribers, plus the pieces shared by the pub-sub
//! implementations: subscription id generation and notification delivery.

use jsonrpsee::{
    core::traits::IdProvider, types::SubscriptionId, SubscriptionMessage,
    SubscriptionSink,
};
use mazze_types::H64;
use serde::Serialize;
use std::{collections::HashMap, ops};

/// Subscription id, as handed out to clients (`0x` + 16 hex digits).
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct Id(String);

impl Id {
    pub fn as_string(&self) -> String { self.0.clone() }

    /// The id the server assigned to an accepted subscription.
    pub fn of(sink: &SubscriptionSink) -> Self {
        match sink.subscription_id() {
            SubscriptionId::Str(s) => Id(s.into_owned()),
            SubscriptionId::Num(n) => Id(n.to_string()),
        }
    }
}

#[cfg(not(test))]
mod random {
    use rand;

    pub type Rng = rand::rngs::OsRng;

    pub fn new() -> Rng { rand::rngs::OsRng }
}

#[cfg(test)]
mod random {
    use rand::SeedableRng;
    use rand_xorshift::XorShiftRng;

    const RNG_SEED: [u8; 16] = [0u8; 16];

    pub type Rng = XorShiftRng;

    pub fn new() -> Rng { Rng::from_seed(RNG_SEED) }
}

/// Generates subscription ids in the format the node has always used:
/// a random 64-bit value as `0x`-prefixed, zero-padded hex
/// (e.g. `"0xcd9832f630ecfd0a"`).
#[derive(Debug)]
pub struct HexIdProvider {
    rand: parking_lot::Mutex<random::Rng>,
}

impl Default for HexIdProvider {
    fn default() -> Self {
        HexIdProvider {
            rand: parking_lot::Mutex::new(random::new()),
        }
    }
}

impl IdProvider for HexIdProvider {
    fn next_id(&self) -> SubscriptionId<'static> {
        let data = H64::random_using(&mut *self.rand.lock());
        SubscriptionId::Str(format!("{:?}", data).into())
    }
}

/// Sends `result` as a subscription notification to `sink`.
pub async fn notify<T: Serialize>(sink: &SubscriptionSink, result: &T) {
    let msg = match serde_json::value::to_raw_value(result) {
        Ok(msg) => SubscriptionMessage::from(msg),
        Err(e) => {
            warn!(target: "rpc", "Unable to serialize notification: {}", e);
            return;
        }
    };
    if let Err(e) = sink.send(msg).await {
        warn!(target: "rpc", "Unable to send notification: {}", e);
    }
}

pub struct Subscribers<T> {
    subscriptions: HashMap<Id, T>,
}

impl<T> Default for Subscribers<T> {
    fn default() -> Self {
        Subscribers {
            subscriptions: HashMap::new(),
        }
    }
}

impl<T> Subscribers<T> {
    /// Adds a subscriber under the id the server assigned to it.
    pub fn insert(&mut self, id: Id, val: T) {
        debug!(target: "pubsub", "Adding subscription id={:?}", id);
        self.subscriptions.insert(id, val);
    }

    /// Removes subscription with given id and returns it (if any).
    pub fn remove(&mut self, id: &Id) -> Option<T> {
        trace!(target: "pubsub", "Removing subscription id={:?}", id);
        self.subscriptions.remove(id)
    }
}

impl<T> ops::Deref for Subscribers<T> {
    type Target = HashMap<Id, T>;

    fn deref(&self) -> &Self::Target { &self.subscriptions }
}

#[cfg(test)]
mod tests {
    use super::HexIdProvider;
    use jsonrpsee::{core::traits::IdProvider, types::SubscriptionId};

    #[test]
    fn ids_are_0x_prefixed_16_hex_digits() {
        let provider = HexIdProvider::default();
        for _ in 0..100 {
            match provider.next_id() {
                SubscriptionId::Str(s) => {
                    assert_eq!(s.len(), 18, "{}", s);
                    assert!(s.starts_with("0x"));
                    assert!(s[2..].chars().all(|c| c.is_ascii_hexdigit()));
                }
                other => panic!("unexpected id {:?}", other),
            }
        }
    }
}
