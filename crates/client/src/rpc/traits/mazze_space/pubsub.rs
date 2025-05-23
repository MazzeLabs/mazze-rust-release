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

use crate::rpc::types::pubsub;
///! Mazze PUB-SUB rpc interface.
use jsonrpc_core::Result;
use jsonrpc_derive::rpc;
use jsonrpc_pubsub::{typed, SubscriptionId};

/// Mazze PUB-SUB rpc interface.
#[rpc(server)]
pub trait PubSub {
    type Metadata;

    /// Subscribes to Mazze subscription.
    #[pubsub(
        subscription = "mazze_subscription",
        subscribe,
        name = "mazze_subscribe"
    )]
    fn subscribe(
        &self, _: Self::Metadata, _: typed::Subscriber<pubsub::Result>,
        _: pubsub::Kind, _: Option<pubsub::Params>,
    );

    /// Unsubscribe from existing Mazze subscription.
    #[pubsub(
        subscription = "mazze_subscription",
        unsubscribe,
        name = "mazze_unsubscribe"
    )]
    fn unsubscribe(
        &self, _: Option<Self::Metadata>, _: SubscriptionId,
    ) -> Result<bool>;
}
