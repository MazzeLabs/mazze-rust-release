// Copyright (c) The Diem Core Contributors
// SPDX-License-Identifier: Apache-2.0

// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use move_core_types::move_resource::MoveResource;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct FreezingBit {
    is_frozen: bool,
}

impl FreezingBit {
    pub fn is_frozen(&self) -> bool {
        self.is_frozen
    }
}

impl MoveResource for FreezingBit {
    const MODULE_NAME: &'static str = "AccountFreezing";
    const STRUCT_NAME: &'static str = "FreezingBit";
}
