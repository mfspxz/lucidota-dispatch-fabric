//! Dispatcher and runtime (T061-T070).
//!
//! Dispatch envelopes, concurrency-limited dispatch, typed timeouts, bounded
//! retries, hedged inference, output validation, in-flight tracking, orphan
//! recovery, idempotent effects, and per-class/per-endpoint metrics.
//!
//! Interface contract frozen by the orchestrator (PHASE 2): `envelope`.
//! Workers implement bodies and own the remaining modules outright.

pub mod dispatch;
pub mod envelope;
pub mod hedge;
pub mod idem;
pub mod metrics;
pub mod orphan;
pub mod output;
pub mod progress;
pub mod retry;
pub mod timeouts;

pub use envelope::{DispatchEnvelope, EnvelopeLimits};
