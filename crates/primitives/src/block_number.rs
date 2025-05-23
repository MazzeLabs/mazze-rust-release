// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

pub fn compute_block_number(
    epoch_start_block_number: u64, block_index_in_epoch: u64,
) -> u64 {
    return epoch_start_block_number + block_index_in_epoch;
}
