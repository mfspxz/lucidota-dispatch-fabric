//! Provenance, events, and durable records (T079-T085).
//!
//! Git policy enforcement, per-attempt provenance capture, content-addressed
//! storage, typed events, hash-chained event logs, transactional attempt
//! receipts, and prompt/prompt-version storage.
//!
//! Interface contract frozen by the orchestrator (PHASE 3): `store`.
//! Workers implement bodies and own the remaining modules outright.

pub mod chain;
pub mod events;
pub mod gitpolicy;
pub mod provenance;
pub mod promptstore;
pub mod receipts;
pub mod store;

pub use store::ContentStore;
