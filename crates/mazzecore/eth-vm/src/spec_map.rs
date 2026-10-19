// Copyright 2025 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

//! Mazze `Spec` ↔ revm `SpecId` mapping for the eSpace VM.
//!
//! Mazze launches eSpace on revm from genesis with no transition cutover, so
//! this module is intentionally trivial: every eSpace transaction runs under
//! the same `SpecId`, and that `SpecId` is the latest stable revm hard fork.
//!
//! Ethereum hard forks are backwards-compatible. A contract compiled for
//! Frontier, Byzantium, Istanbul, London, Shanghai, Cancun, or Prague all
//! execute correctly under `SpecId::PRAGUE`. People can deploy whatever they
//! want.
//!
//! ## Bumping the target spec
//!
//! When revm publishes support for a new Ethereum hard fork:
//!   1. Bump the `revm` version in `Cargo.toml`.
//!   2. Bump the `ESPACE_SPEC` constant below.
//!   3. Re-run the Ethereum state-test harness; it must continue to pass.
//!
//! That is the entire upgrade procedure. There is no per-EIP plumbing, no
//! `Spec` field to thread, no `TransitionsBlockNumber` entry to add.

use revm::primitives::hardfork::SpecId;

/// The eSpace target spec.
///
/// We launch at the latest stable revm `SpecId`. Every Solidity version
/// (including the just-released 0.8.35) deploys and runs without
/// additional work, because newer Ethereum forks remain backwards-compatible
/// with older bytecode.
///
/// To target a newer fork: bump `revm` in `Cargo.toml`, then bump this
/// constant.
pub const ESPACE_SPEC: SpecId = SpecId::PRAGUE;

/// Resolve the revm `SpecId` for a given eSpace block.
///
/// Today this returns `ESPACE_SPEC` unconditionally. The `_block_number`
/// parameter is kept so a future plan can introduce per-block fork gating
/// (e.g. to replay against a historical Ethereum hard fork) without
/// changing the call sites that already pass block number through.
#[inline]
pub fn revm_spec_for(_block_number: u64) -> SpecId {
    ESPACE_SPEC
}
