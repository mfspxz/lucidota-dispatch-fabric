//! Quota and budget engine (T041-T050).
//!
//! Independent limit models, token-bucket admission, sliding-window rate
//! limiting, in-flight enforcement, usage recording, reset windows,
//! budget reservations, fair allocation, adaptive throttling, and dispatch
//! decision records.
//!
//! Interface contract frozen by the orchestrator (PHASE 2): `limits`.
//! Workers implement bodies and own the remaining modules outright.

pub mod adaptive;
pub mod bucket;
pub mod decisions;
pub mod fair;
pub mod inflight;
pub mod limits;
pub mod reserve;
pub mod resets;
pub mod usage;
pub mod window;

pub use limits::{LimitKind, LimitSet, QuotaLimit};

/// Milliseconds since the Unix epoch; clocks are injectable at call sites.
pub type UnixMillis = u64;
