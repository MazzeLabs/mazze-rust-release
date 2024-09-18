// Copyright (c) The Diem Core Contributors
// SPDX-License-Identifier: Apache-2.0

// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use crate::{tests::suite, InMemoryStorage, Storage};

#[test]
fn in_memory() {
    let mut storage = Storage::from(InMemoryStorage::new());
    suite::execute_all_storage_tests(&mut storage);
}
