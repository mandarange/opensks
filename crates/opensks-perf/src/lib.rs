//! OpenSKS runtime performance + lifecycle hardening (PR-043).
//!
//! This crate closes process/task/cache/leak risks with testable invariants:
//!
//! - [`supervisor`] — a typed process supervisor that spawns, tracks, and
//!   deterministically REAPS child processes (no orphans, no leaked file
//!   descriptors), with explicit, unambiguous cancellation ownership.
//! - [`cache`] — a generic [`BoundedLruCache`] with a hard capacity that evicts
//!   the least-recently-used entry at the cap, plus a [`BoundedPageWindow`] that
//!   caps retained pages.
//! - [`batcher`] — a [`BoundedBatcher`] that coalesces a high-rate event stream
//!   into bounded, order-preserving batches without silent loss (drops only via
//!   an explicit, counted overflow policy).
//! - [`stress`] — a harness that drives 100k synthetic events through the
//!   batcher + cache under a supervised run and proves the peak retained memory
//!   stays within a fixed budget regardless of input size.

pub mod batcher;
pub mod cache;
pub mod stress;
pub mod supervisor;

pub use batcher::{Batch, BatcherConfig, BoundedBatcher, OverflowPolicy};
pub use cache::{BoundedLruCache, BoundedPageWindow};
pub use stress::{StressConfig, run_stress};
pub use supervisor::{
    CancelObserver, CancelToken, OsChild, ProcessSupervisor, Reapable, SupervisorError,
};
