// Copyright 2024 Mazze Foundation. All rights reserved.
// DAG-Embedded Tree Structure (DETS) is free software and distributed under
// Apache License 2.0. See https://www.apache.org/licenses/LICENSE-2.0

pub mod message;
pub mod network_event;
pub mod network_sender;
pub mod request_manager;
pub mod sync_protocol;

use network::{service::ProtocolVersion, ProtocolId};

pub const HSB_PROTOCOL_ID: ProtocolId = *b"mzhsb"; // HotStuff Synchronization Protocol
pub const HSB_PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion(1);
