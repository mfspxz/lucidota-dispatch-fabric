//! Driver configuration (DRV-01, owned by cell DRV1).
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// DRV-01: configuration for one driver run. All paths required; the loop
/// bound is explicit — there is no unbounded mode (fail-closed).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriverConfig {
    /// Path to the work-order manifest JSON (leaf_tasks with stable_id,
    /// objective, dependencies, acceptance_predicates).
    pub manifest_path: PathBuf,
    /// Root directory for this run: workspaces, outbox, inbox, records.
    pub work_root: PathBuf,
    /// How often to poll the inbox for receipts, milliseconds.
    pub poll_interval_ms: u64,
    /// Maximum waves before the driver stops with BudgetExhausted.
    pub max_waves: u32,
    /// Maximum time to wait for a wave's receipts, milliseconds.
    /// 0 means wait indefinitely (the runner is responsible for liveness).
    pub wave_timeout_ms: u64,
    /// Optional path to a JSON file with `Vec<VerifierRegistration>`
    /// (fabric-verify). When present, the driver runs registered verifiers
    /// against each receipt's workspace and gates on the VERDICT, not the
    /// receipt. When absent, the driver checks receipt shape + digest
    /// linkage only (documented degradation).
    pub verifiers_path: Option<PathBuf>,
}
