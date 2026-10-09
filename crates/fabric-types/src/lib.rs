//! Foundation types for the agent workforce dispatch system.
//!
//! Implements leaf tasks T001-T010 of `master-spec v2026-10-09`:
//! validated immutable identifiers, task classes, capability requirements,
//! task contracts, state transitions, content-addressed artifact references,
//! evidence receipts, verdicts, provider/credential-reference types, and
//! shared structured errors.
//!
//! Design rules enforced crate-wide:
//! - All identifier types are validated at construction AND deserialization.
//! - Types are immutable: fields are private, no interior mutability.
//! - [`provider::SecretRef`] cannot hold a secret value by construction and
//!   never emits one through `Debug` or serialization.

pub mod artifact;
pub mod capability;
pub mod class;
pub mod contract;
pub mod error;
pub mod ids;
pub mod provider;
pub mod receipt;
pub mod state;
pub mod verdict;

pub use artifact::ArtifactRef;
pub use capability::{CapabilityRequirement, ContextBudget, QualityTier};
pub use class::TaskClass;
pub use contract::{AcceptancePredicate, BackoffKind, RetryPolicy, RiskTier, Scope, TaskContract};
pub use error::{FabricError, Result};
pub use ids::{SpecId, TaskId};
pub use provider::{EndpointId, ModelId, ProviderId, SecretRef};
pub use receipt::EvidenceReceipt;
pub use state::TaskState;
pub use verdict::{FailureKind, Verdict};
