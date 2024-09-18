// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use super::Decision;
use mazze_types::{H256, U64};
use serde_derive::Serialize;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    ///
    pub latest_committed: U64,
    ///
    pub epoch: U64,
    ///
    pub main_decision: Decision,
    ///
    pub latest_voted: Option<U64>,
    ///
    pub latest_tx_number: U64,
}

impl Default for Status {
    fn default() -> Status {
        Status {
            epoch: U64::default(),
            latest_committed: U64::default(),
            main_decision: Decision {
                height: U64::default(),
                block_hash: H256::default(),
            },
            latest_voted: None,
            latest_tx_number: U64::default(),
        }
    }
}
