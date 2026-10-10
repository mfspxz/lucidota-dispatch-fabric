//! DRV-01 — INTERFACE CONTRACT (frozen by orchestrator).
//!
//! The self-hosting build loop. One `run()` call:
//!
//! 1. Loads the manifest, builds the task DAG (fabric-graph).
//! 2. Until no work remains or a terminal outcome:
//!    a. Computes the ready set (fabric-graph ready).
//!    b. For each ready task: creates an isolated workspace
//!       (fabric-workspace), builds a dispatch envelope (fabric-dispatch),
//!       writes it to the outbox, records the dispatch event
//!       (fabric-records).
//!    c. Waits for receipts to appear in the inbox (backend).
//!    d. For each receipt: verifies it (fabric-verify); on pass, records it
//!       (fabric-records) and marks the task's gate passed.
//! 3. Returns a DriverReport: per-task outcomes, waves run, terminal reason.
//!
//! The driver never executes worker logic. Workers are external: they watch
//! the outbox, do the work in the workspace named by the envelope, and drop
//! receipts in the inbox. The driver knows digests, not contents.
//!
//! Worker DRV1: replace the `todo!()` bodies with production implementations.
//! Public signatures are frozen; add private helpers freely. The dead-drop
//! protocol (outbox/inbox file layout) lives in `backend.rs` (your file).

use crate::config::DriverConfig;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DriverError {
    #[error("configuration invalid: {0}")]
    InvalidConfig(String),
    #[error("manifest error: {0}")]
    Manifest(String),
    #[error("graph error: {0}")]
    Graph(String),
    #[error("workspace error: {0}")]
    Workspace(String),
    #[error("dispatch error: {0}")]
    Dispatch(String),
    #[error("backend error: {0}")]
    Backend(String),
    #[error("verification failed for {0}: {1}")]
    VerificationFailed(String, String),
    #[error("record error: {0}")]
    Record(String),
}

/// How the run ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WaveOutcome {
    /// Every task reached a terminal gate verdict.
    Complete,
    /// Tasks remain but none are ready and none are in flight.
    Stalled { reason: String },
    /// max_waves reached with work remaining.
    BudgetExhausted,
    /// wave_timeout_ms elapsed waiting for receipts.
    WaveTimeout,
}

/// Per-task outcome in the final report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskOutcome {
    pub task_id: String,
    pub waves_waited: u32,
    /// "pass" | "fail" | "unstarted" — never invented, always from a receipt
    /// or from the graph state.
    pub gate: String,
    pub receipt_digest: Option<String>,
}

/// DRV-01: what `run()` returns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriverReport {
    pub outcome: WaveOutcome,
    pub waves_run: u32,
    pub tasks: Vec<TaskOutcome>,
    /// Path to the run's record directory (events, receipts, workspaces).
    pub run_dir: PathBuf,
}

/// DRV-01: the self-hosting driver.
pub struct Driver {
    config: DriverConfig,
}

impl Driver {
    /// Validate the config (paths exist, bounds sane) and construct.
    pub fn new(config: DriverConfig) -> Result<Self, DriverError> {
        let _ = config;
        todo!("DRV-01: implement driver construction")
    }

    /// Run the build loop to a terminal outcome.
    pub fn run(&self) -> Result<DriverReport, DriverError> {
        todo!("DRV-01: implement the self-hosting build loop")
    }
}
