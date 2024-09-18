// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use std::sync::atomic::{AtomicU64, Ordering};

pub struct UniqueId {
    next: AtomicU64,
}

impl UniqueId {
    pub fn new() -> Self {
        UniqueId {
            next: AtomicU64::new(0),
        }
    }

    #[inline]
    pub fn next(&self) -> u64 {
        self.next.fetch_add(1, Ordering::Relaxed).into()
    }
}
