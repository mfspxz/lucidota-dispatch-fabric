//! Task graph and queue (T051-T060).
//!
//! Graph nodes and dependency edges, ready-set computation, deterministic
//! priority, critical-path analysis, partitioning, idempotency, backpressure,
//! cancellation propagation, blocked-task diagnostics, and replayable queue
//! snapshots.
//!
//! Interface contract frozen by the orchestrator (PHASE 2): `nodes`.
//! Workers implement bodies and own the remaining modules outright.

pub mod backpressure;
pub mod blocked;
pub mod cancel;
pub mod critical;
pub mod idem;
pub mod nodes;
pub mod partition;
pub mod priority;
pub mod ready;
pub mod snapshot;

pub use nodes::{DepEdge, EdgeKind, TaskGraph, TaskNode};
