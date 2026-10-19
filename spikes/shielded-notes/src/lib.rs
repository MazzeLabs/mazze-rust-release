//! Spike: statistically hiding note commitments, off-chain note delivery,
//! mandatory claim and restore-from-seed for the Mazze shielded space.
//! See docs/internal/privacy-pq-stark.md §4.2 and §9, and README.md here.

pub mod chain;
pub mod keys;
pub mod note;
pub mod primitives;
pub mod seal;
pub mod tree;
pub mod wallet;
