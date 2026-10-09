//! Capability registry (T021-T030).
//!
//! Provider-independent capability catalog, endpoint registration and
//! validation, health tracking, rolling statistics, evidence-based success
//! rates, deterministic capability matching, context estimation, credential
//! resolution via secret references, and versioned immutable snapshots.
//!
//! Interface contracts frozen by the orchestrator (PHASE 0): `endpoints`.
//! Workers implement bodies and own the remaining modules outright.

pub mod catalog;
pub mod context;
pub mod credentials;
pub mod endpoints;
pub mod health;
pub mod matching;
pub mod snapshot;
pub mod stats;
pub mod success;
pub mod validation;

pub use endpoints::{ContextWindow, Endpoint, EndpointRegistry, TransportKind};

/// Milliseconds since the Unix epoch. Clocks are injectable at the call
/// sites that need determinism; this alias keeps the representation uniform.
pub type UnixMillis = u64;
