//! Evidence, verifiers, and verdicts (T086-T100).
//!
//! Response artifact storage, predicate-to-evidence linkage, credential
//! redaction, lineage queries, ledger bundle export/restore, verifier
//! registration and bounded execution, test-output normalization,
//! independent predicate evaluation, coverage reports, independent review
//! dispatch, verification caching, and verdict receipts.
//!
//! Interface contract frozen by the orchestrator (PHASE 3): `registry`.
//! Workers implement bodies and own the remaining modules outright.

pub mod artifacts;
pub mod attest;
pub mod bundle;
pub mod cache;
pub mod coverage;
pub mod exec;
pub mod findings;
pub mod linkage;
pub mod normalize;
pub mod predicates;
pub mod redact;
pub mod slsa;
pub mod registry;
pub mod review;
pub mod verdict;

pub use registry::VerifierRegistry;
