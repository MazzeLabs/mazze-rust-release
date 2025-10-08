// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use crate::impls::errors::Result;
use error_chain::bail;
use std::path::Path;

#[allow(dead_code)]
pub struct KvdbMdbx {
    _unreachable: (),
}

impl KvdbMdbx {
    #[allow(dead_code)]
    pub fn open(_path: &Path) -> Result<Self> {
        bail!("MDBX backend not implemented yet")
    }
}
