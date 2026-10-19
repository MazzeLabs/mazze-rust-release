// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

// Copyright 2015-2020 Parity Technologies (UK) Ltd.
// This file is part of OpenEthereum.

// OpenEthereum is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// OpenEthereum is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with OpenEthereum.  If not, see <http://www.gnu.org/licenses/>.

//! Parity version specific information.

extern crate target_info;

use target_info::Target;

/// Get the platform identifier.
pub fn platform() -> String {
    let env = Target::env();
    let env_dash = if env.is_empty() { "" } else { "-" };
    format!("{}-{}{}{}", Target::arch(), Target::os(), env_dash, env)
}

/// Short commit this binary was built from. Remote fleet builds get their
/// source by rsync and see a stale `.git`, so the build scripts pass the real
/// commit in `MAZZE_BUILD_COMMIT`; vergen's value is only the fallback.
pub fn commit() -> &'static str {
    match option_env!("MAZZE_BUILD_COMMIT") {
        Some(c) if !c.is_empty() => c,
        _ => env!("VERGEN_GIT_SHA"),
    }
}

/// Get the standard version string for this software.
pub fn version(crate_version: &str) -> String {
    let sha3 = commit();
    let sha3_dash = if sha3.is_empty() { "" } else { "-" };
    let commit_date = env!("VERGEN_GIT_COMMIT_DATE").replace("-", "");
    let date_dash = if commit_date.is_empty() { "" } else { "-" };
    format!(
        "mazze-rust/v{}{}{}{}{}/{}/rustc{}",
        crate_version,
        sha3_dash,
        sha3,
        date_dash,
        commit_date,
        platform(),
        env!("VERGEN_RUSTC_SEMVER"),
    )
}
