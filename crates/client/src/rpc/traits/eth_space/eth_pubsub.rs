// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use crate::rpc::types::eth::eth_pubsub as pubsub;
///! Mazze PUB-SUB rpc interface.
use jsonrpc_core::Result;
use jsonrpc_derive::rpc;
use jsonrpc_pubsub::{typed, SubscriptionId};

/// Mazze PUB-SUB rpc interface.
#[rpc(server)]
pub trait EthPubSub {
    type Metadata;

    /// Subscribes to Mazze subscription.
    #[pubsub(
        subscription = "eth_subscription",
        subscribe,
        name = "eth_subscribe"
    )]
    fn subscribe(
        &self, _: Self::Metadata, _: typed::Subscriber<pubsub::Result>,
        _: pubsub::Kind, _: Option<pubsub::Params>,
    );

    /// Unsubscribe from existing Mazze subscription.
    #[pubsub(
        subscription = "eth_subscription",
        unsubscribe,
        name = "eth_unsubscribe"
    )]
    fn unsubscribe(
        &self, _: Option<Self::Metadata>, _: SubscriptionId,
    ) -> Result<bool>;
}
