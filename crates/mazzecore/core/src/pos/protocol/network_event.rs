// Copyright 2024 Mazze Foundation. All rights reserved.
// DAG-Embedded Tree Structure (DETS) is free software and distributed under
// Apache License 2.0. See https://www.apache.org/licenses/LICENSE-2.0

use serde::{Deserialize, Serialize};

/// Container for exchanging transactions with other Mempools.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum NetworkEvent {
    PeerConnected,
    PeerDisconnected,
}
