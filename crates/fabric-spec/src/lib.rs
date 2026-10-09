//! Specification compiler (T011-T020).
//!
//! Turns a versioned master specification into typed requirements, deterministic
//! IDs, traceability graphs, validated task contracts, a dependency DAG, rendered
//! worker prompts, and a machine-readable work-order manifest.
//!
//! Interface contracts frozen by the orchestrator (PHASE 0): `parser`, `dag`.
//! Workers implement bodies and own the remaining modules outright.

pub mod bounds;
pub mod dag;
pub mod ids;
pub mod manifest;
pub mod parser;
pub mod planner;
pub mod predicates;
pub mod render;
pub mod trace;
pub mod validate;

pub use dag::{CycleError, Dag};
pub use parser::{ParsedSpec, Requirement, SourceAnchor};
