//! DRV-01 / DRV-03 — INTERFACE CONTRACT (frozen by orchestrator).
//!
//! The self-hosting build loop. One `run()` call:
//!
//! 1. Loads the manifest, builds the task DAG (fabric-graph).
//! 2. Until no work remains or a terminal outcome, repeat: compute the
//!    ready set (fabric-graph ready); for each ready task create an isolated
//!    workspace (fabric-workspace), build a dispatch envelope
//!    (fabric-dispatch), write it to the outbox, and record the dispatch
//!    event (fabric-records); wait for receipts in the inbox (backend); for
//!    each receipt verify its shape and linkage, record it (fabric-records),
//!    and mark the task's gate.
//! 3. Returns a DriverReport: per-task outcomes, waves run, terminal reason.
//!
//! The driver never executes worker logic. Workers are external: they watch
//! the outbox, do the work in the workspace named by the envelope, and drop
//! receipts in the inbox. The driver knows digests, not contents.
//!
//! Worker DRV1: replace the `todo!()` bodies with production implementations.
//! Public signatures are frozen; add private helpers freely. The dead-drop
//! protocol (outbox/inbox file layout) lives in `backend.rs` (your file).

use crate::backend::{Inbox, Outbox};
use crate::config::DriverConfig;
use fabric_dispatch::envelope::{create_envelope, DispatchEnvelope, EnvelopeLimits};
use fabric_graph::ready::{ready_tasks, GateVerdicts};
use fabric_graph::{DepEdge, EdgeKind, TaskGraph, TaskNode};
use fabric_records::chain::EventLog;
use fabric_records::events::{EventPayload, FabricEvent};
use fabric_records::store::digest_bytes;
use fabric_types::{
    AcceptancePredicate, EndpointId, FabricError, Scope, TaskClass, TaskContract, TaskId, Verdict,
};
use fabric_workspace::workspace::{create_workspace, WorkspaceConfig};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
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

impl From<FabricError> for DriverError {
    fn from(e: FabricError) -> Self {
        DriverError::Record(format!("{e:?}"))
    }
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

// --- Manifest ---------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct ManifestFile {
    #[serde(default)]
    leaf_tasks: Vec<LeafTask>,
}

#[derive(Debug, Deserialize)]
struct LeafTask {
    stable_id: String,
    #[serde(default)]
    objective: String,
    #[serde(default)]
    versioned_spec_reference: String,
    #[serde(default)]
    allowed_scope: Vec<String>,
    #[serde(default)]
    inputs: Vec<String>,
    #[serde(default)]
    outputs: Vec<String>,
    #[serde(default)]
    dependencies: Vec<String>,
    #[serde(default)]
    acceptance_predicates: Vec<String>,
    #[serde(default)]
    verifier_ids: Vec<String>,
}

// --- Driver -----------------------------------------------------------------

/// DRV-01: the self-hosting driver.
pub struct Driver {
    config: DriverConfig,
}

impl Driver {
    /// Validate the config (paths exist, bounds sane) and construct.
    ///
    /// Validation is fail-closed:
    /// - `manifest_path` must exist and be a file (read, not just metadata).
    /// - `work_root` must be creatable (create_dir_all runs now).
    /// - `poll_interval_ms` must be > 0 (a zero poll interval would busy-spin).
    /// - `max_waves` must be >= 1 (there is no unbounded mode).
    ///
    /// `wave_timeout_ms == 0` is allowed: wait indefinitely (the runner owns
    /// liveness in that mode).
    pub fn new(config: DriverConfig) -> Result<Self, DriverError> {
        if !config.manifest_path.is_file() {
            return Err(DriverError::InvalidConfig(format!(
                "manifest_path is not a file: {}",
                config.manifest_path.display()
            )));
        }
        // Fail early on unreadable manifests, not mid-run.
        std::fs::read(&config.manifest_path).map_err(|e| {
            DriverError::InvalidConfig(format!(
                "manifest_path unreadable {}: {e}",
                config.manifest_path.display()
            ))
        })?;
        std::fs::create_dir_all(&config.work_root).map_err(|e| {
            DriverError::InvalidConfig(format!(
                "work_root not creatable {}: {e}",
                config.work_root.display()
            ))
        })?;
        if config.poll_interval_ms == 0 {
            return Err(DriverError::InvalidConfig(
                "poll_interval_ms must be > 0".to_string(),
            ));
        }
        if config.max_waves < 1 {
            return Err(DriverError::InvalidConfig(
                "max_waves must be >= 1".to_string(),
            ));
        }
        Ok(Self { config })
    }

    /// Run the build loop to a terminal outcome.
    ///
    /// DRV-03. Honest loop: every state transition is driven by graph state,
    /// receipt files, or explicit bounds — never by invented verdicts.
    ///
    /// A `work_root` serves one run: a second `run()` on the same root finds
    /// the first run's outbox/inbox/workspaces and fails loudly on duplicate
    /// workspace branches rather than silently overwriting or resuming.
    /// There is no resume mode; start a fresh `work_root` for a new run.
    pub fn run(&self) -> Result<DriverReport, DriverError> {
        let cfg = &self.config;
        let run_dir = cfg.work_root.clone();

        // 1. Load the manifest and build the DAG.
        let (graph, contracts) = load_manifest(&cfg.manifest_path)?;
        let task_order: Vec<TaskId> = graph.nodes().into_iter().map(|n| n.id.clone()).collect();

        // 2. Run-scoped directories.
        let outbox = Outbox::new(&run_dir.join("outbox"))?;
        let inbox = Inbox::new(&run_dir.join("inbox"))?;
        let worktree_root = run_dir.join("workspaces");
        std::fs::create_dir_all(&worktree_root).map_err(|e| {
            DriverError::Workspace(format!("cannot create workspaces dir: {e}"))
        })?;
        let ws_config = ensure_workspace_repo(&run_dir, &worktree_root)?;
        let base_commit = ws_config_base_commit(&ws_config)?;

        let endpoint = EndpointId::new("driver:local").map_err(|e| {
            // The brief named "driver/local", but '/' is outside the
            // identifier charset ([A-Za-z0-9-_.:]); ':' is the honest
            // separator that validates.
            DriverError::Dispatch(format!("fixed endpoint id invalid: {e:?}"))
        })?;
        let limits = EnvelopeLimits {
            timeout_ms: 600_000,
            max_tokens: 8192,
        };
        let output_schema = serde_json::json!({"type": "object"});

        let mut log = EventLog::new();
        let mut gates: GateVerdicts = HashMap::new();
        let mut attempts: HashMap<TaskId, u32> = HashMap::new();
        let mut waves_waited: HashMap<TaskId, u32> = HashMap::new();
        let mut receipt_digests: HashMap<TaskId, String> = HashMap::new();
        let mut waves_run: u32 = 0;

        let outcome = loop {
            // Terminal: every task has a gate verdict.
            if task_order.iter().all(|id| gates.contains_key(id)) {
                break WaveOutcome::Complete;
            }
            // Terminal: wave budget spent.
            if waves_run >= cfg.max_waves {
                break WaveOutcome::BudgetExhausted;
            }

            let ready = ready_tasks(&graph, &gates);
            for id in &task_order {
                if !gates.contains_key(id) {
                    *waves_waited.entry(id.clone()).or_insert(0) += 1;
                }
            }
            // Terminal: work remains, nothing ready, nothing in flight.
            // (Any in-flight receipts were already awaited this wave's
            // wait; a wave that dispatched anything always waits below.)
            if ready.is_empty() {
                break WaveOutcome::Stalled {
                    reason: stall_reason(&graph, &gates, &task_order),
                };
            }

            waves_run += 1;

            // Dispatch every ready task.
            let mut expected: Vec<String> = Vec::new();
            let mut digest_to_task: HashMap<String, TaskId> = HashMap::new();
            for task_id in &ready {
                let attempt = attempts.get(task_id).copied().unwrap_or(0) + 1;
                attempts.insert(task_id.clone(), attempt);

                let ws = create_workspace(&ws_config, &base_commit, task_id, attempt)
                    .map_err(|e| DriverError::Workspace(format!("{task_id}: {e:?}")))?;

                let contract = contracts
                    .get(task_id)
                    .cloned()
                    .ok_or_else(|| DriverError::Graph(format!("no contract for {task_id}")))?;
                let prompt_digest = digest_bytes(contract.objective.as_bytes()).0;

                let envelope: DispatchEnvelope = create_envelope(
                    contract,
                    prompt_digest.clone(),
                    endpoint.clone(),
                    limits,
                    ws.id.clone(),
                    output_schema.clone(),
                    attempt,
                )
                .map_err(|e| DriverError::Dispatch(format!("{task_id}: {e:?}")))?;
                let digest = envelope.envelope_digest();
                outbox.write_envelope(&envelope)?;

                log.append(
                    FabricEvent::new(
                        now_ms(),
                        task_id.clone(),
                        attempt,
                        EventPayload::Dispatch {
                            envelope_digest: digest.clone(),
                            workspace: ws.id,
                        },
                    )
                    .map_err(|e| DriverError::Record(format!("event: {e:?}")))?,
                );
                digest_to_task.insert(digest.clone(), task_id.clone());
                expected.push(digest);
            }
            persist_events(&run_dir, &log)?;

            // Wait for receipts.
            let receipts =
                inbox.wait_for_receipts(&expected, cfg.poll_interval_ms, cfg.wave_timeout_ms)?;
            if receipts.len() < expected.len() {
                break WaveOutcome::WaveTimeout;
            }

            // Verify shape + linkage only; deep verification is the
            // verifier's job. A receipt that parses and names a known
            // envelope digest gates its task with the receipt's verdict.
            for receipt in &receipts {
                let task_id = digest_to_task
                    .get(&receipt.envelope_digest)
                    .cloned()
                    .ok_or_else(|| {
                        DriverError::Backend(format!(
                            "receipt {} names unknown envelope digest {}",
                            receipt.path.display(),
                            receipt.envelope_digest
                        ))
                    })?;
                let verdict = parse_receipt_verdict(receipt)?;
                log.append(
                    FabricEvent::new(
                        now_ms(),
                        task_id.clone(),
                        attempts.get(&task_id).copied().unwrap_or(1),
                        EventPayload::Response {
                            response_digest: receipt.file_digest.clone(),
                            artifact_digest: receipt
                                .body
                                .get("artifact_digests")
                                .and_then(|v| v.as_array())
                                .and_then(|a| a.first())
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string()),
                        },
                    )
                    .map_err(|e| DriverError::Record(format!("event: {e:?}")))?,
                );
                log.append(
                    FabricEvent::new(
                        now_ms(),
                        task_id.clone(),
                        attempts.get(&task_id).copied().unwrap_or(1),
                        EventPayload::Verify { verdict },
                    )
                    .map_err(|e| DriverError::Record(format!("event: {e:?}")))?,
                );
                receipt_digests.insert(task_id.clone(), receipt.file_digest.clone());
                gates.insert(task_id, verdict);
            }
            persist_events(&run_dir, &log)?;
        };

        let tasks = task_order
            .iter()
            .map(|id| {
                let gate = match gates.get(id) {
                    Some(v) if v.is_pass() => "pass",
                    Some(_) => "fail",
                    None => "unstarted",
                }
                .to_string();
                TaskOutcome {
                    task_id: id.as_str().to_string(),
                    waves_waited: waves_waited.get(id).copied().unwrap_or(0),
                    gate,
                    receipt_digest: receipt_digests.get(id).cloned(),
                }
            })
            .collect();

        Ok(DriverReport {
            outcome,
            waves_run,
            tasks,
            run_dir,
        })
    }
}

// --- Manifest loading --------------------------------------------------------

/// Load the manifest, build contracts, and assemble the task DAG.
///
/// Fail-closed: unknown task IDs in `dependencies`, duplicate task IDs, and
/// incomplete contracts are manifest errors naming the offender.
fn load_manifest(
    path: &std::path::Path,
) -> Result<(TaskGraph, HashMap<TaskId, TaskContract>), DriverError> {
    let bytes = std::fs::read(path)
        .map_err(|e| DriverError::Manifest(format!("cannot read {}: {e}", path.display())))?;
    let manifest: ManifestFile = serde_json::from_slice(&bytes)
        .map_err(|e| DriverError::Manifest(format!("invalid manifest JSON: {e}")))?;

    let mut graph = TaskGraph::new();
    let mut contracts: HashMap<TaskId, TaskContract> = HashMap::new();

    // Pre-validate: every dependency names a task present in the manifest.
    // Fail-closed with a Manifest error naming the offender — never a
    // half-built graph.
    let known: std::collections::HashSet<&str> =
        manifest.leaf_tasks.iter().map(|l| l.stable_id.as_str()).collect();
    for leaf in &manifest.leaf_tasks {
        for dep in &leaf.dependencies {
            if !known.contains(dep.as_str()) {
                return Err(DriverError::Manifest(format!(
                    "{} depends on unknown task {}",
                    leaf.stable_id, dep
                )));
            }
        }
    }

    // Pass 1: nodes (edges need both endpoints to exist).
    for leaf in &manifest.leaf_tasks {
        let id = TaskId::new(&leaf.stable_id)
            .map_err(|e| DriverError::Manifest(format!("bad task id {:?}: {e:?}", leaf.stable_id)))?;
        if contracts.contains_key(&id) {
            return Err(DriverError::Manifest(format!("duplicate task id {id}")));
        }
        let deps: Vec<TaskId> = leaf
            .dependencies
            .iter()
            .map(|d| {
                TaskId::new(d)
                    .map_err(|e| DriverError::Manifest(format!("bad dependency id {:?}: {e:?}", d)))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let acceptance: Vec<AcceptancePredicate> = leaf
            .acceptance_predicates
            .iter()
            .map(|p| AcceptancePredicate::new(p, p, true))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| DriverError::Manifest(format!("{id}: bad acceptance predicate: {e:?}")))?;
        // The manifest carries no task class; class is a contract-required
        // field, so the driver defaults to Implement and says so.
        let contract = TaskContract::builder(id.clone(), TaskClass::Implement, &leaf.objective)
            .spec_revision(if leaf.versioned_spec_reference.trim().is_empty() {
                "unspecified"
            } else {
                leaf.versioned_spec_reference.as_str()
            })
            .inputs(leaf.inputs.clone())
            .outputs(leaf.outputs.clone())
            .dependencies(deps.clone())
            .scope(
                Scope::new(leaf.allowed_scope.clone()).map_err(|e| {
                    DriverError::Manifest(format!("{id}: bad scope: {e:?}"))
                })?,
            )
            .acceptance(acceptance)
            .verifiers(leaf.verifier_ids.clone())
            .build()
            .map_err(|e| DriverError::Manifest(format!("{id}: incomplete contract: {e:?}")))?;

        graph
            .add_node(TaskNode {
                id: id.clone(),
                contract: contract.clone(),
            })
            .map_err(|e| DriverError::Graph(format!("{id}: {e:?}")))?;
        contracts.insert(id, contract);
    }

    // Pass 2: dependency edges.
    for leaf in &manifest.leaf_tasks {
        let from = TaskId::new(&leaf.stable_id).map_err(|e| {
            DriverError::Manifest(format!("bad task id {:?}: {e:?}", leaf.stable_id))
        })?;
        let contract = contracts.get(&from).ok_or_else(|| {
            DriverError::Graph(format!("no contract for {from}"))
        })?;
        for dep in &contract.dependencies {
            graph
                .add_edge(DepEdge {
                    from: from.clone(),
                    to: dep.clone(),
                    kind: EdgeKind::DependsOn,
                })
                .map_err(|e| DriverError::Graph(format!("{from} -> {dep}: {e:?}")))?;
        }
    }
    Ok((graph, contracts))
}

// --- Workspaces -----------------------------------------------------------------

/// Ensure the driver's workspace repo exists and return its config.
/// The repo lives at `<run_dir>/.driver-repo`; worktrees go under
/// `<run_dir>/workspaces/`. The repo is created on first run (init + one
/// empty commit as the pin base) and reused afterwards.
fn ensure_workspace_repo(
    run_dir: &std::path::Path,
    worktree_root: &std::path::Path,
) -> Result<WorkspaceConfig, DriverError> {
    let repo = run_dir.join(".driver-repo");
    if !repo.join(".git").exists() && !repo.join("HEAD").exists() {
        std::fs::create_dir_all(&repo)
            .map_err(|e| DriverError::Workspace(format!("cannot create repo dir: {e}")))?;
        git(&repo, &["init", "-q", "-b", "main"])?;
        git(&repo, &["config", "user.email", "driver@fabric"])?;
        git(&repo, &["config", "user.name", "fabric-driver"])?;
        git(&repo, &["config", "commit.gpgsign", "false"])?;
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "driver base"])?;
    }
    Ok(WorkspaceConfig {
        repo_path: repo,
        worktree_root: worktree_root.to_path_buf(),
        branch_prefix: "driver".to_string(),
    })
}

/// Resolve the workspace repo's HEAD as the pin base commit.
fn ws_config_base_commit(config: &WorkspaceConfig) -> Result<String, DriverError> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(&config.repo_path)
        .args(["rev-parse", "HEAD"])
        .output()
        .map_err(|e| DriverError::Workspace(format!("git rev-parse failed to spawn: {e}")))?;
    if !out.status.success() {
        return Err(DriverError::Workspace("workspace repo has no HEAD".to_string()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn git(repo: &std::path::Path, args: &[&str]) -> Result<(), DriverError> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|e| DriverError::Workspace(format!("git failed to spawn: {e}")))?;
    if !out.status.success() {
        return Err(DriverError::Workspace(format!(
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(())
}

// --- Receipts -------------------------------------------------------------------

/// Shape-and-linkage check for one receipt. The filename<->embedded-digest
/// linkage was already verified by the inbox. Here: the receipt must carry a
/// `verdict` the system understands; anything else is a verification
/// failure naming the digest — deep verification is the verifier's job.
fn parse_receipt_verdict(
    receipt: &crate::backend::ReceiptFile,
) -> Result<Verdict, DriverError> {
    let raw = receipt.body.get("verdict").and_then(|v| v.as_str());
    match raw.map(str::to_lowercase).as_deref() {
        Some("pass") => Ok(Verdict::Pass),
        Some("fail") => Ok(Verdict::Fail),
        Some("inconclusive") => Ok(Verdict::Inconclusive),
        Some("blocked") => Ok(Verdict::Blocked),
        other => Err(DriverError::VerificationFailed(
            receipt.envelope_digest.clone(),
            format!("receipt verdict unrecognized: {other:?}"),
        )),
    }
}

// --- Events ---------------------------------------------------------------------

fn persist_events(run_dir: &std::path::Path, log: &EventLog) -> Result<(), DriverError> {
    let path = run_dir.join("events.jsonl");
    let mut out = String::new();
    for record in log.records() {
        let line = serde_json::to_string(record)
            .map_err(|e| DriverError::Record(format!("event serialization: {e}")))?;
        out.push_str(&line);
        out.push('\n');
    }
    std::fs::write(&path, out)
        .map_err(|e| DriverError::Record(format!("cannot write {}: {e}", path.display())))?;
    Ok(())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Machine-readable reason for a Stalled outcome: which tasks are un-gated
/// and which unpassed prerequisites hold each one back.
fn stall_reason(graph: &TaskGraph, gates: &GateVerdicts, order: &[TaskId]) -> String {
    let mut parts = Vec::new();
    for id in order {
        if gates.contains_key(id) {
            continue;
        }
        let blockers: Vec<String> = graph
            .prerequisites(id)
            .into_iter()
            .filter(|p| !gates.get(p).is_some_and(|v| v.is_pass()))
            .map(|p| p.as_str().to_string())
            .collect();
        parts.push(format!("{id} blocked by [{}]", blockers.join(", ")));
    }
    format!("{} task(s) un-gated, none ready: {}", parts.len(), parts.join("; "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DriverConfig;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("drv1-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn manifest_two_tasks(dir: &std::path::Path) -> PathBuf {
        let leaf = |id: &str, deps: &[&str]| {
            serde_json::json!({
                "stable_id": id,
                "objective": format!("objective of {id}"),
                "versioned_spec_reference": "rev-1",
                "allowed_scope": ["x"],
                "inputs": ["in"],
                "outputs": ["out"],
                "dependencies": deps,
                "acceptance_predicates": [format!("{id}.done")],
                "verifier_ids": ["v"],
            })
        };
        let m = serde_json::json!({ "leaf_tasks": [leaf("T001", &[]), leaf("T002", &["T001"])] });
        let p = dir.join("manifest.json");
        std::fs::write(&p, serde_json::to_vec(&m).unwrap()).unwrap();
        p
    }

    fn config_for(dir: &std::path::Path, manifest: &std::path::Path) -> DriverConfig {
        DriverConfig {
            manifest_path: manifest.to_path_buf(),
            work_root: dir.join("run"),
            poll_interval_ms: 10,
            max_waves: 5,
            wave_timeout_ms: 10_000,
        }
    }

    #[test]
    fn new_rejects_bad_configs() {
        let d = tmpdir("cfg");
        let good_manifest = manifest_two_tasks(&d);
        let good = config_for(&d, &good_manifest);
        assert!(Driver::new(good).is_ok());

        // Manifest missing.
        let mut bad = config_for(&d, &good_manifest);
        bad.manifest_path = d.join("nope.json");
        assert!(matches!(
            Driver::new(bad),
            Err(DriverError::InvalidConfig(_))
        ));

        // Zero poll interval.
        let mut bad = config_for(&d, &good_manifest);
        bad.poll_interval_ms = 0;
        assert!(matches!(
            Driver::new(bad),
            Err(DriverError::InvalidConfig(_))
        ));

        // Zero max waves.
        let mut bad = config_for(&d, &good_manifest);
        bad.max_waves = 0;
        assert!(matches!(
            Driver::new(bad),
            Err(DriverError::InvalidConfig(_))
        ));

        // Uncreatable work root (a file, not a dir).
        let blocker = d.join("blocker");
        std::fs::write(&blocker, b"x").unwrap();
        let mut bad = config_for(&d, &good_manifest);
        bad.work_root = blocker.join("sub");
        assert!(matches!(
            Driver::new(bad),
            Err(DriverError::InvalidConfig(_))
        ));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn run_completes_two_task_chain_with_preseeded_receipts() {
        let d = tmpdir("run2");
        let manifest = manifest_two_tasks(&d);
        let driver = Driver::new(config_for(&d, &manifest)).unwrap();

        // Wave 1: dispatch T001 only (T002 blocked). Seed its receipt, then
        // run to completion by seeding each wave's outbox before the wait.
        // Because run() is one call, we seed in a helper thread that watches
        // the outbox and answers every envelope with a pass receipt.
        let run_dir = d.join("run");
        let seeder = std::thread::spawn({
            let run_dir = run_dir.clone();
            move || {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
                loop {
                    let outbox = run_dir.join("outbox");
                    if outbox.is_dir() {
                        for entry in std::fs::read_dir(&outbox).unwrap() {
                            let entry = entry.unwrap();
                            let name = entry.file_name().to_str().unwrap().to_string();
                            if !name.ends_with(".json") || name.ends_with(".receipt.json") {
                                continue;
                            }
                            let digest = name.trim_end_matches(".json").to_string();
                            let rp = run_dir.join("inbox").join(format!("{digest}.receipt.json"));
                            if !rp.exists() {
                                let body = serde_json::json!({
                                    "envelope_digest": digest,
                                    "verdict": "pass",
                                    "artifact_digests": ["sha256:seed"],
                                });
                                write_atomic_test(&rp, &serde_json::to_vec(&body).unwrap());
                            }
                        }
                    }
                    if std::time::Instant::now() > deadline {
                        break;
                    }
                    if run_dir.join("DONE").exists() {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }
        });

        let report = driver.run().unwrap();
        std::fs::write(run_dir.join("DONE"), b"1").unwrap();
        seeder.join().unwrap();

        assert_eq!(report.outcome, WaveOutcome::Complete);
        assert_eq!(report.waves_run, 2);
        assert_eq!(report.tasks.len(), 2);
        for t in &report.tasks {
            assert_eq!(t.gate, "pass", "{t:?}");
            assert!(t.receipt_digest.as_deref().is_some_and(|s| s.starts_with("sha256:")));
        }
        // Outbox holds exactly two envelopes; inbox two receipts.
        let n_out = std::fs::read_dir(run_dir.join("outbox")).unwrap().count();
        let n_in = std::fs::read_dir(run_dir.join("inbox")).unwrap().count();
        assert_eq!(n_out, 2);
        assert_eq!(n_in, 2);
        // Events were recorded and chained: per task, 1 dispatch +
        // 1 response + 1 verify.
        let events = std::fs::read_to_string(run_dir.join("events.jsonl")).unwrap();
        assert_eq!(events.lines().count(), 6);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn wave_timeout_when_no_worker_answers() {
        let d = tmpdir("timeout");
        let manifest = manifest_two_tasks(&d);
        let mut cfg = config_for(&d, &manifest);
        cfg.wave_timeout_ms = 300;
        let driver = Driver::new(cfg).unwrap();
        let report = driver.run().unwrap();
        assert_eq!(report.outcome, WaveOutcome::WaveTimeout);
        assert_eq!(report.waves_run, 1);
        assert!(report.tasks.iter().all(|t| t.gate == "unstarted"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn budget_exhausted_when_max_waves_reached() {
        let d = tmpdir("budget");
        let manifest = manifest_two_tasks(&d);
        let mut cfg = config_for(&d, &manifest);
        cfg.max_waves = 1;
        cfg.wave_timeout_ms = 300;
        // Seed receipts so wave 1 completes T001; T002 needs wave 2.
        let run_dir = d.join("run");
        let seeder = std::thread::spawn({
            let run_dir = run_dir.clone();
            move || {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
                loop {
                    let outbox = run_dir.join("outbox");
                    if outbox.is_dir() {
                        for entry in std::fs::read_dir(&outbox).unwrap() {
                            let entry = entry.unwrap();
                            let name = entry.file_name().to_str().unwrap().to_string();
                            if !name.ends_with(".json") || name.ends_with(".receipt.json") {
                                continue;
                            }
                            let digest = name.trim_end_matches(".json").to_string();
                            let rp = run_dir.join("inbox").join(format!("{digest}.receipt.json"));
                            if !rp.exists() {
                                let body = serde_json::json!({
                                    "envelope_digest": digest,
                                    "verdict": "pass",
                                });
                                write_atomic_test(&rp, &serde_json::to_vec(&body).unwrap());
                            }
                        }
                    }
                    if std::time::Instant::now() > deadline || run_dir.join("DONE").exists() {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }
        });
        let driver = Driver::new(cfg).unwrap();
        let report = driver.run().unwrap();
        std::fs::write(run_dir.join("DONE"), b"1").unwrap();
        seeder.join().unwrap();
        assert_eq!(report.outcome, WaveOutcome::BudgetExhausted);
        assert_eq!(report.waves_run, 1);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn stalled_when_dag_has_a_cycle() {
        let d = tmpdir("stalled");
        // Cyclic DAG: nothing can ever become ready, nothing gets a verdict.
        let m = serde_json::json!({ "leaf_tasks": [
            {
                "stable_id": "T001", "objective": "o1", "versioned_spec_reference": "r",
                "allowed_scope": ["x"], "inputs": ["i"], "outputs": ["o"],
                "dependencies": ["T002"], "acceptance_predicates": ["p1"], "verifier_ids": ["v"],
            },
            {
                "stable_id": "T002", "objective": "o2", "versioned_spec_reference": "r",
                "allowed_scope": ["x"], "inputs": ["i"], "outputs": ["o"],
                "dependencies": ["T001"], "acceptance_predicates": ["p2"], "verifier_ids": ["v"],
            },
        ]});
        let manifest = d.join("manifest.json");
        std::fs::write(&manifest, serde_json::to_vec(&m).unwrap()).unwrap();
        let driver = Driver::new(config_for(&d, &manifest)).unwrap();
        let report = driver.run().unwrap();
        assert!(
            matches!(report.outcome, WaveOutcome::Stalled { .. }),
            "{:?}",
            report.outcome
        );
        assert_eq!(report.waves_run, 0);
        assert!(report.tasks.iter().all(|t| t.gate == "unstarted"));
        if let WaveOutcome::Stalled { reason } = &report.outcome {
            assert!(reason.contains("T001") && reason.contains("T002"), "{reason}");
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn fail_verdict_recorded_as_fail_gate() {
        let d = tmpdir("failgate");
        let m = serde_json::json!({ "leaf_tasks": [
            {
                "stable_id": "T001", "objective": "o1", "versioned_spec_reference": "r",
                "allowed_scope": ["x"], "inputs": ["i"], "outputs": ["o"],
                "dependencies": [], "acceptance_predicates": ["p1"], "verifier_ids": ["v"],
            },
        ]});
        let manifest = d.join("manifest.json");
        std::fs::write(&manifest, serde_json::to_vec(&m).unwrap()).unwrap();
        let mut cfg = config_for(&d, &manifest);
        cfg.max_waves = 1;
        let driver = Driver::new(cfg).unwrap();
        let run_dir = d.join("run");
        let seeder = std::thread::spawn({
            let run_dir = run_dir.clone();
            move || {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
                loop {
                    let outbox = run_dir.join("outbox");
                    if outbox.is_dir() {
                        for entry in std::fs::read_dir(&outbox).unwrap() {
                            let entry = entry.unwrap();
                            let name = entry.file_name().to_str().unwrap().to_string();
                            if !name.ends_with(".json") || name.ends_with(".receipt.json") {
                                continue;
                            }
                            let digest = name.trim_end_matches(".json").to_string();
                            let rp = run_dir.join("inbox").join(format!("{digest}.receipt.json"));
                            if !rp.exists() {
                                let body = serde_json::json!({
                                    "envelope_digest": digest,
                                    "verdict": "fail",
                                });
                                write_atomic_test(&rp, &serde_json::to_vec(&body).unwrap());
                            }
                        }
                    }
                    if std::time::Instant::now() > deadline || run_dir.join("DONE").exists() {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }
        });
        let report = driver.run().unwrap();
        std::fs::write(run_dir.join("DONE"), b"1").unwrap();
        seeder.join().unwrap();
        // T001 failed in wave 1. A fail verdict gates the task (gate=fail);
        // the driver does not invent retry policy, so the run completes.
        assert_eq!(report.outcome, WaveOutcome::Complete);
        let t1 = &report.tasks[0];
        assert_eq!(t1.gate, "fail");
        assert!(t1.receipt_digest.as_deref().is_some_and(|s| s.starts_with("sha256:")));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Atomic file write for test seeders: a worker must never leave a
    /// half-written receipt where the driver's inbox poll can see it.
    fn write_atomic_test(path: &std::path::Path, bytes: &[u8]) {
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, bytes).unwrap();
        std::fs::rename(&tmp, path).unwrap();
    }

    #[test]
    fn unknown_dependency_is_manifest_error() {
        let d = tmpdir("baddag");
        let m = serde_json::json!({ "leaf_tasks": [
            {
                "stable_id": "T001", "objective": "o1", "versioned_spec_reference": "r",
                "allowed_scope": ["x"], "inputs": ["i"], "outputs": ["o"],
                "dependencies": ["T999"], "acceptance_predicates": ["p1"], "verifier_ids": ["v"],
            },
        ]});
        let manifest = d.join("manifest.json");
        std::fs::write(&manifest, serde_json::to_vec(&m).unwrap()).unwrap();
        let driver = Driver::new(config_for(&d, &manifest)).unwrap();
        assert!(matches!(driver.run(), Err(DriverError::Manifest(_))));
        let _ = std::fs::remove_dir_all(&d);
    }
}
