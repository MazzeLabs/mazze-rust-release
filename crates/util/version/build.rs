extern crate anyhow;
extern crate vergen;

use anyhow::Result;
use vergen::EmitBuilder;

fn main() -> Result<()> {
    println!("cargo:rerun-if-env-changed=MAZZE_BUILD_COMMIT");
    // Emits VERGEN_GIT_SHA (short form), VERGEN_GIT_COMMIT_DATE
    // (YYYY-MM-DD) and VERGEN_RUSTC_SEMVER.
    EmitBuilder::builder()
        .git_sha(true)
        .git_commit_date()
        .rustc_semver()
        .emit()
}
