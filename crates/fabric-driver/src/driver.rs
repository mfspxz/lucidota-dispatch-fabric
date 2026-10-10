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
//!
//! DRV-04 (cell DRV2): verifier wiring. When `DriverConfig::verifiers_path`
//! is `Some`, the driver loads `Vec<VerifierRegistration>` at startup
//! (malformed file = `InvalidConfig`, fail-closed) and gates each task on a
//! real verifier VERDICT: after collecting a wave's receipts it looks up
//! verifiers for the task's class + each acceptance predicate's capability,
//! executes them bounded against the task's workspace, evaluates predicates
//! with `fabric_verify::predicates::evaluate`, and finalizes a
//! `VerdictBuilder`. Gate = pass only if the verdict passes. When
//! `verifiers_path` is `None` the driver keeps the degraded behavior
//! (receipt shape + digest linkage only) and records the degradation in the
//! report.
//!
//! DRV-05 (cell DRV2): `TaskOutcome` carries `verified: bool` and
//! `verdict_digest: Option<String>`; `DriverReport` carries
//! `verification_degraded: bool`. Verifier-mode verdict receipts are written
//! to `<run_dir>/verdicts/<task>-a<attempt>.json`.

use crate::backend::{Inbox, Outbox};
use crate::config::DriverConfig;
use fabric_dispatch::envelope::{create_envelope, DispatchEnvelope, EnvelopeLimits};
use fabric_graph::ready::{ready_tasks, GateVerdicts};
use fabric_graph::{DepEdge, EdgeKind, TaskGraph, TaskNode};
use fabric_records::chain::{EventLog, EventRecord};
use fabric_records::events::{EventPayload, FabricEvent};
use fabric_records::store::digest_bytes;
use fabric_spec::render::{render_prompt, RenderedPrompt, SharedContracts};
use fabric_types::{
    AcceptancePredicate, BackoffKind, EndpointId, FabricError, RetryPolicy, Scope, TaskClass,
    TaskContract, TaskId, Verdict,
};
use fabric_verify::exec::{execute, ExecConfig, ExecOutcome};
use fabric_verify::predicates::{
    evaluate, AcceptancePredicate as VerifyPredicate, EvidenceLink, OnMissing, PredicateOutcome,
};
use fabric_verify::registry::{VerifierCapability, VerifierRegistration, VerifierRegistry};
use fabric_verify::verdict::{PredicateVerdict, VerdictBuilder};
use fabric_workspace::workspace::{create_workspace, WorkspaceConfig};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
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
    /// True when registered verifiers ran and the gate reflects their
    /// verdict. False in degraded mode (receipt shape + linkage only).
    pub verified: bool,
    /// The `VerdictReceipt.receipt_digest` when `verified`; None when
    /// degraded.
    pub verdict_digest: Option<String>,
    /// Set when the task's receipt was quarantined (malformed, linkage
    /// mismatch, or over the size cap): the human-readable cause naming
    /// the file. None otherwise.
    pub receipt_error: Option<String>,
}

/// DRV-01: what `run()` returns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriverReport {
    pub outcome: WaveOutcome,
    pub waves_run: u32,
    pub tasks: Vec<TaskOutcome>,
    /// Path to the run's record directory (events, receipts, workspaces).
    pub run_dir: PathBuf,
    /// True when `verifiers_path` was `None`: gates came from receipt claims
    /// (shape + digest linkage), not from verifier verdicts.
    pub verification_degraded: bool,
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
    /// Optional bound on dispatch attempts for this task. Absent means the
    /// contract default: exactly one attempt (`RetryPolicy::once`). The
    /// driver never retries silently beyond this bound.
    #[serde(default)]
    max_attempts: Option<u32>,
}

// --- Driver -----------------------------------------------------------------

/// Render the T019 compartmented worker prompt for one task contract.
///
/// * `contract` — the task contract (the worker's whole world).
/// * `output_schema` — the JSON schema the worker's output must satisfy.
/// * `shared` — shared-contract identifiers for the run (fabric version and
///   the manifest/policy digest).
///
/// The driver holds no requirement corpus, so requirements are empty by
/// construction. Deterministic: same inputs → byte-identical text, and
/// `RenderedPrompt.digest` is `sha256:` over the exact rendered bytes.
fn render_worker_prompt(
    contract: &TaskContract,
    output_schema: &serde_json::Value,
    shared: &SharedContracts,
) -> Result<RenderedPrompt, String> {
    render_prompt(contract, &[], shared, output_schema)
        .map_err(|e| format!("render_prompt: {e:?}"))
}

/// Atomically write the rendered worker prompt into the task workspace as
/// `PROMPT.md` (tmp file + rename; a worker must never see a half-written
/// prompt).
fn write_prompt_to_workspace(
    workspace_path: &std::path::Path,
    prompt_text: &str,
) -> Result<(), String> {
    let path = workspace_path.join("PROMPT.md");
    let tmp = workspace_path.join("PROMPT.md.tmp");
    std::fs::write(&tmp, prompt_text)
        .map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("rename {}: {e}", path.display()))?;
    Ok(())
}

/// DRV-01: the self-hosting driver.
pub struct Driver {
    config: DriverConfig,
    /// Loaded verifier registry when `verifiers_path` was `Some`.
    /// `None` = degraded mode (receipt shape + linkage only).
    verifiers: Option<VerifierRegistry>,
}

impl Driver {
    /// Validate the config (paths exist, bounds sane) and construct.
    ///
    /// Validation is fail-closed:
    /// - `manifest_path` must exist and be a file (read, not just metadata).
    /// - `work_root` must be creatable (create_dir_all runs now).
    /// - `max_waves` must be >= 1 (there is no unbounded mode).
    ///
    /// `wave_timeout_ms == 0` is allowed: wait indefinitely (the runner owns
    /// liveness in that mode). Receipt waiting is event-driven (filesystem
    /// notifications), never timer-polled — there is no poll interval to
    /// configure.
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
        if config.max_waves < 1 {
            return Err(DriverError::InvalidConfig(
                "max_waves must be >= 1".to_string(),
            ));
        }
        let verifiers = match &config.verifiers_path {
            Some(path) => Some(load_verifiers(path)?),
            None => None,
        };
        // Fail closed on degraded mode: without verifiers, gates would follow
        // unverified receipt claims. The operator must either configure
        // `verifiers_path` or explicitly opt in with `allow_degraded`.
        if verifiers.is_none() && !config.allow_degraded {
            return Err(DriverError::InvalidConfig(
                "verifiers_path is not set and allow_degraded is false: refusing to run in degraded mode, where task gates would follow unverified receipt claims. Configure verifiers_path, or set allow_degraded=true to explicitly opt in to degraded mode.".to_string(),
            ));
        }
        Ok(Self { config, verifiers })
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

        // The manifest is the run's governing policy: its content digest
        // pins the policy the worker prompts were rendered under. Re-read
        // here (it was validated in `new`) so the digest covers exactly the
        // bytes the DAG was built from.
        let manifest_bytes = std::fs::read(&cfg.manifest_path).map_err(|e| {
            DriverError::Manifest(format!(
                "cannot re-read {}: {e}",
                cfg.manifest_path.display()
            ))
        })?;
        let policy_digest = digest_bytes(&manifest_bytes).0;
        let shared = SharedContracts {
            fabric_version: env!("CARGO_PKG_VERSION").to_string(),
            policy_digest,
        };

        let mut log = load_event_log(&run_dir)?;
        let mut gates: GateVerdicts = HashMap::new();
        let mut attempts: HashMap<TaskId, u32> = HashMap::new();
        let mut waves_waited: HashMap<TaskId, u32> = HashMap::new();
        let mut receipt_digests: HashMap<TaskId, String> = HashMap::new();
        let mut workspace_paths: HashMap<TaskId, PathBuf> = HashMap::new();
        let mut verified_flags: HashMap<TaskId, bool> = HashMap::new();
        let mut verdict_digests: HashMap<TaskId, String> = HashMap::new();
        let mut receipt_errors: HashMap<TaskId, String> = HashMap::new();
        // FIX-3: tasks whose retry policy is exhausted. They keep their
        // terminal fail gate but are never redispatched and never appear
        // in a ready set again.
        let mut retired: HashSet<TaskId> = HashSet::new();
        let mut waves_run: u32 = 0;
        let verification_degraded = self.verifiers.is_none();

        let outcome = loop {
            // Terminal: every task reached a terminal gate verdict (pass,
            // or fail with no retries left).
            if task_order
                .iter()
                .all(|id| gate_terminal(&gates, &contracts, &attempts, id))
            {
                break WaveOutcome::Complete;
            }
            // Terminal: wave budget spent.
            if waves_run >= cfg.max_waves {
                break WaveOutcome::BudgetExhausted;
            }

            let ready: Vec<TaskId> = ready_tasks(&graph, &gates)
                .into_iter()
                .filter(|id| !retired.contains(id))
                .collect();
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
                let contract = contracts.get(task_id).cloned().ok_or_else(|| {
                    DriverError::Graph(format!("no contract for {task_id}"))
                })?;
                let attempt = attempts.get(task_id).copied().unwrap_or(0) + 1;
                // FIX-3: honor the contract's retry policy. Attempts beyond
                // max_attempts are terminal: no new workspace, no envelope,
                // no redispatch. The gate goes fail and the task retires.
                if !attempt_allowed(&contract, attempt) {
                    gates.insert(task_id.clone(), Verdict::Fail);
                    retired.insert(task_id.clone());
                    continue;
                }
                attempts.insert(task_id.clone(), attempt);

                let ws = create_workspace(&ws_config, &base_commit, task_id, attempt)
                    .map_err(|e| DriverError::Workspace(format!("{task_id}: {e:?}")))?;
                workspace_paths.insert(task_id.clone(), ws.path.clone());

                // Render the T019 compartmented worker prompt. The driver holds
                // no requirement corpus, so requirements are empty by
                // construction — the prompt carries the contract, the
                // identifiers, the output schema, and the RULES, and nothing
                // else. The rendered text is dropped into the task workspace
                // as PROMPT.md (the worker's whole world); the envelope's
                // prompt_digest pins its exact bytes.
                let rendered =
                    render_worker_prompt(&contract, &output_schema, &shared).map_err(|e| {
                        DriverError::Dispatch(format!("{task_id}: prompt render: {e}"))
                    })?;
                write_prompt_to_workspace(&ws.path, &rendered.text).map_err(|e| {
                    DriverError::Dispatch(format!("{task_id}: prompt write: {e}"))
                })?;
                let prompt_digest = rendered.digest;

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

            // Wait for receipts (event-driven; no polling).
            let (receipts, problems) = inbox.wait_for_receipts(&expected, cfg.wave_timeout_ms)?;
            if receipts.len() + problems.len() < expected.len() {
                break WaveOutcome::WaveTimeout;
            }

            // FIX-3: quarantine. A malformed receipt (corrupt JSON, linkage
            // mismatch, over the size cap) fails its task — recorded in the
            // report — instead of aborting the run. One buggy worker cannot
            // DoS the run. The task may still be retried within its retry
            // policy; a later good receipt clears the quarantine record.
            for problem in &problems {
                let task_id = digest_to_task
                    .get(&problem.envelope_digest)
                    .cloned()
                    .ok_or_else(|| {
                        DriverError::Backend(format!(
                            "receipt {} names unknown envelope digest {}",
                            problem.path.display(),
                            problem.envelope_digest
                        ))
                    })?;
                gates.insert(task_id.clone(), Verdict::Fail);
                receipt_errors.insert(task_id, problem.error.clone());
            }

            // Gate each receipt. Degraded mode: shape + linkage only, the
            // gate follows the receipt's claimed verdict. Verifier mode: the
            // receipt's claim is ignored and the gate follows the verifiers'
            // verdict over the task's workspace.
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
                let attempt = attempts.get(&task_id).copied().unwrap_or(1);
                let (verdict, verified, verdict_digest) = match &self.verifiers {
                    Some(registry) => {
                        let contract = contracts.get(&task_id).ok_or_else(|| {
                            DriverError::Graph(format!("no contract for {task_id}"))
                        })?;
                        let ws_path = workspace_paths.get(&task_id).ok_or_else(|| {
                            DriverError::Workspace(format!("no workspace path for {task_id}"))
                        })?;
                        let vr = verify_with_verifiers(registry, contract, ws_path)?;
                        write_verdict_receipt(&run_dir, &task_id, attempt, &vr)?;
                        (
                            vr.overall,
                            true,
                            Some(vr.receipt_digest.clone()),
                        )
                    }
                    None => {
                        let verdict = parse_receipt_verdict(receipt)?;
                        (verdict, false, None)
                    }
                };
                log.append(
                    FabricEvent::new(
                        now_ms(),
                        task_id.clone(),
                        attempt,
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
                        attempt,
                        EventPayload::Verify { verdict },
                    )
                    .map_err(|e| DriverError::Record(format!("event: {e:?}")))?,
                );
                receipt_digests.insert(task_id.clone(), receipt.file_digest.clone());
                receipt_errors.remove(&task_id);
                verified_flags.insert(task_id.clone(), verified);
                if let Some(d) = verdict_digest {
                    verdict_digests.insert(task_id.clone(), d);
                }
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
                    verified: verified_flags.get(id).copied().unwrap_or(false),
                    verdict_digest: verdict_digests.get(id).cloned(),
                    receipt_error: receipt_errors.get(id).cloned(),
                }
            })
            .collect();

        Ok(DriverReport {
            outcome,
            waves_run,
            tasks,
            run_dir,
            verification_degraded,
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
        //
        // Retry policy: honored by the dispatch loop (FIX-3). Absent in the
        // manifest means exactly one attempt. Backoff is accepted by the
        // type but not implemented by the driver: waves are event-driven
        // and the driver never sleeps between attempts (no-timers
        // doctrine), so attempts are separated by waves, not delays.
        let retry = match leaf.max_attempts {
            None => RetryPolicy::once(),
            Some(n) => RetryPolicy::new(n, BackoffKind::None, 0, n.saturating_sub(1)).map_err(|e| {
                DriverError::Manifest(format!("{id}: bad max_attempts: {e:?}"))
            })?,
        };
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
            .retry(retry)
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
    // Canonicalize to absolute paths: `git -C <repo> worktree add <path>`
    // resolves a relative <path> against the repo dir, not the process cwd.
    // A relative worktree_root would nest worktrees inside the repo itself.
    let repo = repo.canonicalize().map_err(|e| {
        DriverError::Workspace(format!("cannot canonicalize repo dir: {e}"))
    })?;
    let worktree_root = worktree_root.canonicalize().map_err(|e| {
        DriverError::Workspace(format!("cannot canonicalize worktree root: {e}"))
    })?;
    Ok(WorkspaceConfig {
        repo_path: repo,
        worktree_root,
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

// --- Verifier wiring (DRV-04) ----------------------------------------------------

/// Load `Vec<VerifierRegistration>` from the configured JSON file into a
/// registry. Any problem — unreadable file, invalid JSON, rejected
/// registration — is `InvalidConfig`: the driver never starts in a
/// half-configured verifier mode.
fn load_verifiers(path: &std::path::Path) -> Result<VerifierRegistry, DriverError> {
    let bytes = std::fs::read(path).map_err(|e| {
        DriverError::InvalidConfig(format!(
            "verifiers_path unreadable {}: {e}",
            path.display()
        ))
    })?;
    let regs: Vec<VerifierRegistration> = serde_json::from_slice(&bytes).map_err(|e| {
        DriverError::InvalidConfig(format!(
            "verifiers_path {} is not a Vec<VerifierRegistration>: {e}",
            path.display()
        ))
    })?;
    let mut registry = VerifierRegistry::new();
    for reg in regs {
        let id = reg.id.clone();
        registry.register(reg).map_err(|e| {
            DriverError::InvalidConfig(format!("verifier registration '{id}' rejected: {e:?}"))
        })?;
    }
    Ok(registry)
}

/// Capability matching rule: a predicate id like `"file-exists:one.txt"`
/// matches verifiers whose capability is exactly `"file-exists"` — the part
/// before the first `':'`. A predicate id with no `':'` matches verifiers
/// whose capability equals the whole id. Matching is string equality, never
/// prefix or substring.
///
/// The part after the first `':'`, when present, is the predicate's
/// parameter and is appended to the verifier's argv as a single argument
/// (no shell interpretation — argv only). This binds the evidence to the
/// specific subject: a verifier for capability `file-exists` invoked as
/// `[script, "one.txt"]` can only produce evidence about `one.txt`, so an
/// exit-0 run for the wrong file can never satisfy this predicate. Predicate
/// ids come from the operator-authored manifest, never from worker output,
/// so parameter injection by a worker is not possible.
fn capability_of(predicate_id: &str) -> &str {
    predicate_id.split(':').next().unwrap_or(predicate_id)
}

/// The predicate parameter: the part after the first `':'`, if any.
fn predicate_param(predicate_id: &str) -> Option<&str> {
    predicate_id.split_once(':').map(|(_, param)| param)
}

/// Run the registered verifiers for one task attempt and produce the
/// verdict that gates the task.
///
/// For each acceptance predicate: look up verifiers for
/// (task class, `capability_of(predicate id)`), execute each one bounded
/// against the task's workspace, and turn every clean exit-0 run into one
/// evidence link keyed by the capability. A verifier that fails its tests
/// or fails infrastructurally produces no link — the predicate then
/// evaluates per its `on_missing` policy, which is `Fail` when evidence is
/// required, so a failed verifier can never gate a pass. `on_missing` is
/// never mapped to PASS.
///
/// The receipt's claimed verdict is deliberately ignored here: the gate
/// follows the verifiers, not the worker.
fn verify_with_verifiers(
    registry: &VerifierRegistry,
    contract: &TaskContract,
    workspace_path: &std::path::Path,
) -> Result<fabric_verify::verdict::VerdictReceipt, DriverError> {
    use fabric_verify::verdict::VerdictReceipt;
    let task = contract.id.as_str().to_string();
    if contract.acceptance.is_empty() {
        // Unreachable: manifest loading requires non-empty acceptance, but
        // fail typed rather than panic if the invariant ever breaks.
        return Err(DriverError::VerificationFailed(
            task,
            "no acceptance predicates to verify".to_string(),
        ));
    }
    let mut builder = VerdictBuilder::expect_predicates(
        &task,
        contract
            .acceptance
            .iter()
            .map(|p| p.id.clone())
            .collect(),
    )
    .map_err(|e| DriverError::VerificationFailed(task.clone(), format!("verdict setup: {e:?}")))?;

    for pred in &contract.acceptance {
        let cap = capability_of(&pred.id);
        let verifiers = registry.lookup(&contract.class, &VerifierCapability(cap.to_string()));
        let mut links: Vec<EvidenceLink> = Vec::new();
        let mut first_command: Vec<String> = Vec::new();
        let mut first_version = "none".to_string();
        for (i, reg) in verifiers.iter().enumerate() {
            // The verifier's argv is its registered command plus the
            // predicate parameter (if any): the evidence an exit-0 run
            // produces is thereby bound to this predicate's subject.
            let mut argv = reg.command.clone();
            if let Some(param) = predicate_param(&pred.id) {
                argv.push(param.to_string());
            }
            if i == 0 {
                first_command = argv.clone();
                first_version = reg.version.clone();
            }
            // The child environment starts empty (env_allowlist is empty):
            // secret-bearing variables never reach a verifier by default.
            let outcome = execute(&ExecConfig {
                argv,
                workdir: Some(workspace_path.to_path_buf()),
                env_allowlist: Vec::new(),
                timeout_ms: reg.timeout_ms,
                max_output_bytes: reg.max_output_bytes,
            })
            .map_err(|e| {
                DriverError::VerificationFailed(task.clone(), format!("verifier exec: {e:?}"))
            })?;
            // Only a clean exit-0 run becomes evidence. TestFailed and
            // InfraFailed produce no link: the predicate then fails (or
            // goes inconclusive) per its missing-evidence policy.
            if let ExecOutcome::Passed(record) = outcome {
                links.push(EvidenceLink {
                    kind: cap.to_string(),
                    reference: digest_bytes(&record.stdout).0,
                });
            }
        }
        let vp = VerifyPredicate {
            id: pred.id.clone(),
            description: pred.description.clone(),
            required_evidence_kinds: if pred.evidence_required {
                vec![cap.to_string()]
            } else {
                Vec::new()
            },
            on_missing: OnMissing::Fail,
        };
        let outcome = match evaluate(&vp, &links) {
            PredicateOutcome::Pass { .. } => Verdict::Pass,
            PredicateOutcome::Fail { .. } => Verdict::Fail,
            PredicateOutcome::Inconclusive { .. } => Verdict::Inconclusive,
        };
        builder
            .record(PredicateVerdict {
                predicate_id: pred.id.clone(),
                outcome,
                command: first_command,
                verifier_version: first_version,
                evidence_digests: links.iter().map(|l| l.reference.clone()).collect(),
            })
            .map_err(|e| {
                DriverError::VerificationFailed(task.clone(), format!("verdict record: {e:?}"))
            })?;
    }
    let receipt: VerdictReceipt = builder
        .finalize()
        .map_err(|e| DriverError::VerificationFailed(task.clone(), format!("verdict: {e:?}")))?;
    Ok(receipt)
}

/// Persist one verdict receipt under `<run_dir>/verdicts/<task>-a<attempt>.json`.
fn write_verdict_receipt(
    run_dir: &std::path::Path,
    task_id: &TaskId,
    attempt: u32,
    receipt: &fabric_verify::verdict::VerdictReceipt,
) -> Result<(), DriverError> {
    let dir = run_dir.join("verdicts");
    std::fs::create_dir_all(&dir).map_err(|e| {
        DriverError::Record(format!("cannot create verdicts dir {}: {e}", dir.display()))
    })?;
    let path = dir.join(format!("{}-a{}.json", task_id.as_str(), attempt));
    let bytes = serde_json::to_vec_pretty(receipt)
        .map_err(|e| DriverError::Record(format!("verdict serialization: {e}")))?;
    // Atomic: a reader never sees a half-written receipt.
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, &bytes)
        .map_err(|e| DriverError::Record(format!("cannot write {}: {e}", tmp.display())))?;
    std::fs::rename(&tmp, &path)
        .map_err(|e| DriverError::Record(format!("cannot publish {}: {e}", path.display())))?;
    Ok(())
}

// --- Events ---------------------------------------------------------------------

fn persist_events(run_dir: &std::path::Path, log: &EventLog) -> Result<(), DriverError> {
    // Never persist a broken chain: fail closed rather than cementing a
    // tampered or corrupted log to disk.
    log.verify().map_err(|e| {
        DriverError::Record(format!("refusing to persist broken event chain: {e:?}"))
    })?;
    let path = run_dir.join("events.jsonl");
    let mut out = String::new();
    for record in log.records() {
        let line = serde_json::to_string(record)
            .map_err(|e| DriverError::Record(format!("event serialization: {e}")))?;
        out.push_str(&line);
        out.push('\n');
    }
    // Atomic: tmp + rename. A crash leaves either the old log or the new
    // log, never a torn one — same protocol as verdict receipts.
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, &out)
        .map_err(|e| DriverError::Record(format!("cannot write {}: {e}", tmp.display())))?;
    std::fs::rename(&tmp, &path)
        .map_err(|e| DriverError::Record(format!("cannot publish {}: {e}", path.display())))?;
    Ok(())
}

/// Load a previously persisted event log, verifying its hash chain.
///
/// Missing file → fresh empty log (first run into this directory). Present
/// file → every line must parse as an `EventRecord` and the chain must
/// verify; anything else is a typed, fail-closed error naming the file.
///
/// This is the choke point for RT3-001: `EventLog::verify()` previously had
/// zero production callers, so a tampered `events.jsonl` was undetectable.
/// Every run now passes through here before its first append.
///
/// Trust statement: the chain detects tampering of recorded events after the
/// fact; it does not authenticate the writer. The dead-drop inbox is a known
/// limitation here — anyone who could write the log could also have
/// recomputed a valid chain. The chain is a tripwire against silent
/// alteration, not proof of authorship.
fn load_event_log(run_dir: &std::path::Path) -> Result<EventLog, DriverError> {
    let path = run_dir.join("events.jsonl");
    if !path.exists() {
        return Ok(EventLog::new());
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|e| DriverError::Record(format!("cannot read {}: {e}", path.display())))?;
    let mut records = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let record: EventRecord = serde_json::from_str(line).map_err(|e| {
            DriverError::Record(format!(
                "{} line {}: unparsable event record: {e}",
                path.display(),
                i + 1
            ))
        })?;
        records.push(record);
    }
    EventLog::from_records(records).map_err(|e| {
        DriverError::Record(format!(
            "{}: event chain verification failed (log was altered): {e:?}",
            path.display()
        ))
    })
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// FIX-3: retry bound. Attempt numbers are 1-based; the contract's
/// `retry.max_attempts` is the last attempt the driver will dispatch.
/// The builder default is exactly one attempt (`RetryPolicy::once()`); the
/// manifest may raise it via `max_attempts`. The `.max(1)` is defensive:
/// a zero bound would silently refuse the first attempt, which is never
/// the operator's intent — fail the gate instead of dispatching nothing.
fn attempt_allowed(contract: &TaskContract, attempt: u32) -> bool {
    attempt <= contract.retry.max_attempts.max(1)
}

/// FIX-3: a task's gate is terminal for the run when it passed, or when it
/// failed and no attempts remain under its retry policy. A fail with
/// attempts remaining is NOT terminal — the run continues so the retry can
/// be dispatched. (Without this, a lone failed task would end the run
/// before its policy-allowed retry.) Stateless: derived from attempts used
/// vs the contract's `max_attempts`, so it holds even when `max_waves` (or
/// anything else) prevents a later dispatch wave from running.
fn gate_terminal(
    gates: &GateVerdicts,
    contracts: &HashMap<TaskId, TaskContract>,
    attempts: &HashMap<TaskId, u32>,
    id: &TaskId,
) -> bool {
    match gates.get(id) {
        Some(v) if v.is_pass() => true,
        Some(_) => {
            let used = attempts.get(id).copied().unwrap_or(0);
            let max = contracts
                .get(id)
                .map(|c| c.retry.max_attempts.max(1))
                .unwrap_or(1);
            used >= max
        }
        None => false,
    }
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
            max_waves: 5,
            wave_timeout_ms: 10_000,
            verifiers_path: None,
            // Existing tests exercise degraded mode; the opt-in is explicit.
            allow_degraded: true,
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
        // Degraded mode: no verifiers configured.
        assert!(report.verification_degraded);
        for t in &report.tasks {
            assert_eq!(t.gate, "pass", "{t:?}");
            assert!(t.receipt_digest.as_deref().is_some_and(|s| s.starts_with("sha256:")));
            assert!(!t.verified, "{t:?}");
            assert!(t.verdict_digest.is_none(), "{t:?}");
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

    /// Seeder that answers every envelope with a fail-claim receipt. In
    /// degraded mode the gate follows the claim, so the task fails every
    /// attempt — the retry-policy bound is what stops redispatch.
    fn spawn_fail_seeder(run_dir: PathBuf) -> std::thread::JoinHandle<()> {
        spawn_raw_seeder(run_dir, |digest| {
            serde_json::to_vec(&serde_json::json!({
                "envelope_digest": digest,
                "verdict": "fail",
            }))
            .unwrap()
        })
    }

    /// Seeder that answers every envelope with corrupt (non-JSON) bytes.
    fn spawn_corrupt_seeder(run_dir: PathBuf) -> std::thread::JoinHandle<()> {
        spawn_raw_seeder(run_dir, |_digest| b"{{{ not json".to_vec())
    }

    /// Seeder that answers every envelope with an over-the-cap receipt.
    fn spawn_oversize_seeder(run_dir: PathBuf) -> std::thread::JoinHandle<()> {
        spawn_raw_seeder(run_dir, |_digest| vec![b'x'; 9 * 1024 * 1024])
    }

    fn spawn_raw_seeder(
        run_dir: PathBuf,
        body: fn(&str) -> Vec<u8>,
    ) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
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
                            write_atomic_test(&rp, &body(&digest));
                        }
                    }
                }
                if std::time::Instant::now() > deadline || run_dir.join("DONE").exists() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        })
    }

    /// Atomic file write for test seeders: a worker must never leave a
    /// half-written receipt where the driver's inbox watcher can see it.
    fn write_atomic_test(path: &std::path::Path, bytes: &[u8]) {
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, bytes).unwrap();
        std::fs::rename(&tmp, path).unwrap();
    }

    #[test]
    fn capability_matching_rule() {
        // The part before the first ':'; no ':' means the whole id.
        assert_eq!(capability_of("file-exists:one.txt"), "file-exists");
        assert_eq!(capability_of("cargo-test"), "cargo-test");
        assert_eq!(capability_of("a:b:c"), "a");
        assert_eq!(capability_of(""), "");
    }

    fn write_verifiers_file(dir: &std::path::Path, command: &[&str]) -> PathBuf {
        let reg = VerifierRegistration {
            id: "v-file-exists".to_string(),
            classes: vec![TaskClass::Implement],
            capability: VerifierCapability("file-exists".to_string()),
            command: command.iter().map(|s| s.to_string()).collect(),
            version: "1".to_string(),
            timeout_ms: 5_000,
            max_output_bytes: 65_536,
        };
        let p = dir.join("verifiers.json");
        std::fs::write(&p, serde_json::to_vec(&vec![reg]).unwrap()).unwrap();
        p
    }

    fn manifest_capability_predicate(dir: &std::path::Path) -> PathBuf {
        let m = serde_json::json!({ "leaf_tasks": [
            {
                "stable_id": "T001", "objective": "write out.txt",
                "versioned_spec_reference": "r",
                "allowed_scope": ["out.txt"], "inputs": ["i"], "outputs": ["out.txt"],
                "dependencies": [], "acceptance_predicates": ["file-exists:out.txt"],
                "verifier_ids": ["v"],
            },
        ]});
        let p = dir.join("manifest.json");
        std::fs::write(&p, serde_json::to_vec(&m).unwrap()).unwrap();
        p
    }

    /// Seeder shared by verifier-mode tests: answers every envelope with a
    /// pass-claim receipt. In verifier mode the claim is ignored; the gate
    /// follows the verifiers.
    fn spawn_pass_seeder(run_dir: PathBuf) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
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
        })
    }

    #[test]
    fn malformed_verifiers_file_is_invalid_config() {
        let d = tmpdir("badver");
        let manifest = manifest_two_tasks(&d);

        // Garbage JSON.
        let bad = d.join("bad.json");
        std::fs::write(&bad, b"{not json").unwrap();
        let mut cfg = config_for(&d, &manifest);
        cfg.verifiers_path = Some(bad);
        assert!(matches!(
            Driver::new(cfg),
            Err(DriverError::InvalidConfig(_))
        ));

        // Valid JSON, wrong shape.
        let bad = d.join("shape.json");
        std::fs::write(&bad, br#"{"nope": 1}"#).unwrap();
        let mut cfg = config_for(&d, &manifest);
        cfg.verifiers_path = Some(bad);
        assert!(matches!(
            Driver::new(cfg),
            Err(DriverError::InvalidConfig(_))
        ));

        // Registration the registry rejects (empty id).
        let bad = d.join("reg.json");
        let reg = serde_json::json!([{
            "id": "", "classes": ["IMPLEMENT"],
            "capability": "file-exists", "command": ["true"],
            "version": "1", "timeout_ms": 1000, "max_output_bytes": 1024,
        }]);
        std::fs::write(&bad, serde_json::to_vec(&reg).unwrap()).unwrap();
        let mut cfg = config_for(&d, &manifest);
        cfg.verifiers_path = Some(bad);
        assert!(matches!(
            Driver::new(cfg),
            Err(DriverError::InvalidConfig(_))
        ));

        // Missing file.
        let mut cfg = config_for(&d, &manifest);
        cfg.verifiers_path = Some(d.join("missing.json"));
        assert!(matches!(
            Driver::new(cfg),
            Err(DriverError::InvalidConfig(_))
        ));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn verifier_mode_pass_verdict_gates_pass() {
        let d = tmpdir("verpass");
        let manifest = manifest_capability_predicate(&d);
        let verifiers = write_verifiers_file(&d, &["sh", "-c", "exit 0"]);
        let mut cfg = config_for(&d, &manifest);
        cfg.verifiers_path = Some(verifiers);
        // Verifier mode needs no degraded opt-in.
        cfg.allow_degraded = false;
        let driver = Driver::new(cfg).unwrap();

        let run_dir = d.join("run");
        let seeder = spawn_pass_seeder(run_dir.clone());
        let report = driver.run().unwrap();
        std::fs::write(run_dir.join("DONE"), b"1").unwrap();
        seeder.join().unwrap();

        assert_eq!(report.outcome, WaveOutcome::Complete);
        assert!(!report.verification_degraded);
        let t = &report.tasks[0];
        // The gate follows the verifier, not the receipt claim.
        assert_eq!(t.gate, "pass", "{t:?}");
        assert!(t.verified, "{t:?}");
        assert!(
            t.verdict_digest.as_deref().is_some_and(|s| s.starts_with("fnv1a64:")),
            "{t:?}"
        );
        // The verdict receipt was persisted.
        let vf = run_dir.join("verdicts").join("T001-a1.json");
        assert!(vf.is_file(), "missing {vf:?}");
        let vr: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&vf).unwrap()).unwrap();
        assert_eq!(vr["overall"], "PASS");
        assert_eq!(vr["predicates"][0]["predicate_id"], "file-exists:out.txt");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn verifier_mode_failing_verifier_keeps_gate_unpassed() {
        let d = tmpdir("verfail");
        let manifest = manifest_capability_predicate(&d);
        let verifiers = write_verifiers_file(&d, &["sh", "-c", "exit 1"]);
        let mut cfg = config_for(&d, &manifest);
        cfg.verifiers_path = Some(verifiers);
        // Verifier mode needs no degraded opt-in.
        cfg.allow_degraded = false;
        cfg.max_waves = 1;
        let driver = Driver::new(cfg).unwrap();

        let run_dir = d.join("run");
        let seeder = spawn_pass_seeder(run_dir.clone());
        let report = driver.run().unwrap();
        std::fs::write(run_dir.join("DONE"), b"1").unwrap();
        seeder.join().unwrap();

        // The worker claimed pass, but the verifier failed: the gate must
        // not be pass. The driver does not invent retries; the fail verdict
        // gates the task.
        let t = &report.tasks[0];
        assert_eq!(t.gate, "fail", "{t:?}");
        assert!(t.verified, "{t:?}");
        assert!(t.verdict_digest.is_some(), "{t:?}");
        let _ = std::fs::remove_dir_all(&d);
    }

    fn manifest_single_task_retry(dir: &std::path::Path, max_attempts: u32) -> PathBuf {
        let m = serde_json::json!({ "leaf_tasks": [
            {
                "stable_id": "T001",
                "objective": "objective of T001",
                "versioned_spec_reference": "rev-1",
                "allowed_scope": ["x"], "inputs": ["i"], "outputs": ["o"],
                "dependencies": [], "acceptance_predicates": ["p1"], "verifier_ids": ["v"],
                "max_attempts": max_attempts,
            },
        ]});
        let p = dir.join("manifest.json");
        std::fs::write(&p, serde_json::to_vec(&m).unwrap()).unwrap();
        p
    }

    #[test]
    fn attempt_allowed_honors_max_attempts() {
        let contract_with = |n: u32| {
            TaskContract::builder(TaskId::new("T1").unwrap(), TaskClass::Implement, "o")
                .spec_revision("r")
                .inputs(vec!["i".to_string()])
                .outputs(vec!["o".to_string()])
                .scope(Scope::new(vec!["x".to_string()]).unwrap())
                .acceptance(vec![AcceptancePredicate::new("p", "p", true).unwrap()])
                .verifiers(vec!["v".to_string()])
                .retry(RetryPolicy::new(n, BackoffKind::None, 0, 0).unwrap())
                .build()
                .unwrap()
        };
        let c2 = contract_with(2);
        assert!(attempt_allowed(&c2, 1));
        assert!(attempt_allowed(&c2, 2));
        assert!(!attempt_allowed(&c2, 3));
        // Builder default is once(): a single attempt, then stop.
        let c1 = TaskContract::builder(TaskId::new("T1").unwrap(), TaskClass::Implement, "o")
            .spec_revision("r")
            .inputs(vec!["i".to_string()])
            .outputs(vec!["o".to_string()])
            .scope(Scope::new(vec!["x".to_string()]).unwrap())
            .acceptance(vec![AcceptancePredicate::new("p", "p", true).unwrap()])
            .verifiers(vec!["v".to_string()])
            .build()
            .unwrap();
        assert!(attempt_allowed(&c1, 1));
        assert!(!attempt_allowed(&c1, 2));
    }

    #[test]
    fn retry_policy_bounds_redispatch_attempts() {
        // A deterministically-failing task with max_attempts=2 gets exactly
        // 2 attempts (2 workspaces), then a terminal fail gate — not
        // max_waves attempts.
        let d = tmpdir("retrybound");
        let manifest = manifest_single_task_retry(&d, 2);
        let mut cfg = config_for(&d, &manifest);
        cfg.max_waves = 5;
        let driver = Driver::new(cfg).unwrap();

        let run_dir = d.join("run");
        let seeder = spawn_fail_seeder(run_dir.clone());
        let report = driver.run().unwrap();
        std::fs::write(run_dir.join("DONE"), b"1").unwrap();
        seeder.join().unwrap();

        let t = &report.tasks[0];
        assert_eq!(t.gate, "fail", "{t:?}");
        assert!(t.receipt_error.is_none(), "{t:?}");
        // Exactly 2 attempts: no third workspace, no third envelope.
        let ws_count = std::fs::read_dir(run_dir.join("workspaces"))
            .unwrap()
            .count();
        assert_eq!(ws_count, 2, "expected exactly 2 workspaces");
        assert_eq!(report.outcome, WaveOutcome::Complete);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn malformed_receipt_quarantines_task_not_run() {
        // One corrupt receipt fails its task with the cause recorded; the
        // run itself completes with a report.
        let d = tmpdir("quarantine");
        let manifest = manifest_single_task_retry(&d, 1);
        let driver = Driver::new(config_for(&d, &manifest)).unwrap();

        let run_dir = d.join("run");
        let seeder = spawn_corrupt_seeder(run_dir.clone());
        let report = driver.run().unwrap();
        std::fs::write(run_dir.join("DONE"), b"1").unwrap();
        seeder.join().unwrap();

        let t = &report.tasks[0];
        assert_eq!(t.gate, "fail", "{t:?}");
        let err = t.receipt_error.as_deref().expect("quarantine cause recorded");
        assert!(err.contains("fails to parse"), "{err}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn oversize_receipt_quarantined_at_driver() {
        // A 9 MiB receipt is rejected by the size cap and quarantined; the
        // run completes and names the cause.
        let d = tmpdir("bigreceipt");
        let manifest = manifest_single_task_retry(&d, 1);
        let driver = Driver::new(config_for(&d, &manifest)).unwrap();

        let run_dir = d.join("run");
        let seeder = spawn_oversize_seeder(run_dir.clone());
        let report = driver.run().unwrap();
        std::fs::write(run_dir.join("DONE"), b"1").unwrap();
        seeder.join().unwrap();

        let t = &report.tasks[0];
        assert_eq!(t.gate, "fail", "{t:?}");
        let err = t.receipt_error.as_deref().expect("quarantine cause recorded");
        assert!(err.contains("exceeds size cap"), "{err}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn verifier_param_binding_wrong_file_does_not_pass() {
        // Regression: the predicate parameter (the part after ':') is
        // appended to the verifier argv, binding the evidence to the
        // specific subject. A workspace holding the WRONG file must not
        // satisfy "file-exists:out.txt".
        let d = tmpdir("verparam");
        let ws = d.join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(ws.join("wrong.txt"), b"decoy").unwrap();

        let mut registry = VerifierRegistry::new();
        registry
            .register(VerifierRegistration {
                id: "v-param".to_string(),
                classes: vec![TaskClass::Implement],
                capability: VerifierCapability("file-exists".to_string()),
                command: vec!["test".to_string(), "-f".to_string()],
                version: "1".to_string(),
                timeout_ms: 5_000,
                max_output_bytes: 65_536,
            })
            .unwrap();

        let contract = TaskContract::builder(
            TaskId::new("T001").unwrap(),
            TaskClass::Implement,
            "write out.txt",
        )
        .spec_revision("test")
        .inputs(vec!["i".to_string()])
        .outputs(vec!["out.txt".to_string()])
        .scope(Scope::new(vec!["out.txt".to_string()]).unwrap())
        .verifiers(vec!["v-param".to_string()])
        .acceptance(vec![AcceptancePredicate::new(
            "file-exists:out.txt",
            "out.txt exists",
            true,
        )
        .unwrap()])
        .build()
        .unwrap();

        let receipt = verify_with_verifiers(&registry, &contract, &ws).unwrap();
        assert_ne!(
            receipt.overall,
            fabric_types::Verdict::Pass,
            "workspace with only wrong.txt must not pass file-exists:out.txt"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Minimal contract for prompt-render tests.
    fn prompt_test_contract() -> TaskContract {
        TaskContract::builder(
            TaskId::new("T001").unwrap(),
            TaskClass::Implement,
            "write out.txt",
        )
        .spec_revision("test")
        .inputs(vec!["i".to_string()])
        .outputs(vec!["out.txt".to_string()])
        .scope(Scope::new(vec!["out.txt".to_string()]).unwrap())
        .verifiers(vec!["v".to_string()])
        .acceptance(vec![AcceptancePredicate::new("file-exists:out.txt", "out.txt exists", true)
            .unwrap()])
        .build()
        .unwrap()
    }

    fn prompt_test_shared() -> SharedContracts {
        SharedContracts {
            fabric_version: "test".to_string(),
            policy_digest: "sha256:test".to_string(),
        }
    }

    #[test]
    fn render_worker_prompt_contains_rules_and_is_deterministic() {
        let contract = prompt_test_contract();
        let schema = serde_json::json!({"type": "object"});
        let shared = prompt_test_shared();
        let p1 = render_worker_prompt(&contract, &schema, &shared).unwrap();
        let p2 = render_worker_prompt(&contract, &schema, &shared).unwrap();
        // The T019 compartmented RULES reach the worker prompt.
        assert!(p1.text.contains("## RULES"), "prompt missing RULES section");
        assert!(
            p1.text.contains("compartmented"),
            "prompt missing compartmentation rule"
        );
        // Deterministic: same inputs → byte-identical text and digest.
        assert_eq!(p1.text, p2.text);
        assert_eq!(p1.digest, p2.digest);
        // The digest pins the exact rendered bytes.
        assert_eq!(p1.digest, digest_bytes(p1.text.as_bytes()).0);
    }

    #[test]
    fn new_rejects_degraded_without_opt_in() {
        let d = tmpdir("degopt");
        let manifest = manifest_two_tasks(&d);
        let mut cfg = config_for(&d, &manifest);
        cfg.verifiers_path = None;
        cfg.allow_degraded = false;
        let err = match Driver::new(cfg) {
            Ok(_) => panic!("Driver::new must reject degraded mode without opt-in"),
            Err(e) => e,
        };
        let msg = format!("{err}");
        assert!(msg.contains("degraded"), "error must name degraded mode: {msg}");
        assert!(
            msg.contains("verifiers_path"),
            "error must tell the operator what to configure: {msg}"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn degraded_opt_in_run_flags_degraded() {
        // allow_degraded=true preserves the old behavior: the run proceeds
        // on receipt claims and the report flags verification_degraded.
        let d = tmpdir("degflag");
        let manifest = manifest_capability_predicate(&d);
        let mut cfg = config_for(&d, &manifest);
        cfg.verifiers_path = None;
        cfg.allow_degraded = true;
        cfg.max_waves = 1;
        let driver = Driver::new(cfg).unwrap();
        let run_dir = d.join("run");
        let seeder = spawn_pass_seeder(run_dir.clone());
        let report = driver.run().unwrap();
        std::fs::write(run_dir.join("DONE"), b"1").unwrap();
        seeder.join().unwrap();
        assert!(report.verification_degraded, "degraded run must be flagged");
        assert_eq!(report.tasks[0].gate, "pass");
        assert!(!report.tasks[0].verified);
        assert!(report.tasks[0].verdict_digest.is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn prompt_written_to_workspace_pins_digest() {
        // End-to-end: the rendered prompt lands in the task workspace as
        // PROMPT.md, and the envelope's prompt_digest pins its exact bytes.
        let d = tmpdir("promptws");
        let manifest = manifest_capability_predicate(&d);
        let mut cfg = config_for(&d, &manifest);
        cfg.verifiers_path = None;
        cfg.allow_degraded = true;
        cfg.max_waves = 1;
        let driver = Driver::new(cfg).unwrap();
        let run_dir = d.join("run");
        let seeder = spawn_pass_seeder(run_dir.clone());
        let report = driver.run().unwrap();
        std::fs::write(run_dir.join("DONE"), b"1").unwrap();
        seeder.join().unwrap();
        assert_eq!(report.outcome, WaveOutcome::Complete);

        let prompt_path = run_dir.join("workspaces").join("T001-a1").join("PROMPT.md");
        assert!(prompt_path.is_file(), "missing {prompt_path:?}");
        let text = std::fs::read_to_string(&prompt_path).unwrap();
        assert!(text.contains("## RULES"), "PROMPT.md missing compartmented RULES");

        let outbox = run_dir.join("outbox");
        let mut found = false;
        for entry in std::fs::read_dir(&outbox).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().to_str().unwrap().to_string();
            if name.ends_with(".json") && !name.ends_with(".receipt.json") {
                let env: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(entry.path()).unwrap()).unwrap();
                assert_eq!(env["prompt_digest"], digest_bytes(text.as_bytes()).0);
                found = true;
            }
        }
        assert!(found, "no envelope found in outbox");
        let _ = std::fs::remove_dir_all(&d);
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

    // --- Event-log repair tests (FIX-2) -------------------------------------

    fn fix2_event(ts: u64) -> FabricEvent {
        FabricEvent::new(
            ts,
            TaskId::new("T001").unwrap(),
            1,
            EventPayload::Release {
                reason: "fix2".to_string(),
            },
        )
        .unwrap()
    }

    fn fix2_log(n: u64) -> EventLog {
        let mut log = EventLog::new();
        for i in 0..n {
            log.append(fix2_event(i + 1));
        }
        log
    }

    #[test]
    fn tampered_event_file_fails_load_with_typed_error() {
        let d = tmpdir("evtamper");
        let run_dir = d.join("run");
        std::fs::create_dir_all(&run_dir).unwrap();
        persist_events(&run_dir, &fix2_log(3)).unwrap();

        // Flip one byte in the middle of the file.
        let path = run_dir.join("events.jsonl");
        let mut bytes = std::fs::read(&path).unwrap();
        let mid = bytes.len() / 2;
        bytes[mid] ^= 0x01;
        std::fs::write(&path, &bytes).unwrap();

        let err = load_event_log(&run_dir).unwrap_err();
        let msg = format!("{err}");
        assert!(
            matches!(err, DriverError::Record(_)),
            "expected typed Record error, got: {msg}"
        );
        assert!(msg.contains("events.jsonl"), "error must name the file: {msg}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn valid_event_chain_loads_and_verifies() {
        let d = tmpdir("evload");
        let run_dir = d.join("run");
        std::fs::create_dir_all(&run_dir).unwrap();
        let log = fix2_log(3);
        let head = log.head_hash();
        persist_events(&run_dir, &log).unwrap();

        let loaded = load_event_log(&run_dir).unwrap();
        assert_eq!(loaded.records().len(), 3);
        assert_eq!(loaded.head_hash(), head);
        // Appends continue the chain at the right seq.
        let mut loaded = loaded;
        let r = loaded.append(fix2_event(4));
        assert_eq!(r.seq, 3);
        assert!(loaded.verify().is_ok());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn missing_event_file_loads_as_empty_log() {
        let d = tmpdir("evmissing");
        let run_dir = d.join("run");
        std::fs::create_dir_all(&run_dir).unwrap();
        let loaded = load_event_log(&run_dir).unwrap();
        assert_eq!(loaded.records().len(), 0);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn persist_crash_simulation_leaves_old_log_intact() {
        let d = tmpdir("evcrash");
        let run_dir = d.join("run");
        std::fs::create_dir_all(&run_dir).unwrap();
        persist_events(&run_dir, &fix2_log(2)).unwrap();
        let old_bytes = std::fs::read(run_dir.join("events.jsonl")).unwrap();

        // Simulate a crash mid-write: a tmp file exists, the rename never
        // happened. The published log must be untouched.
        std::fs::write(run_dir.join("events.tmp"), b"{\"torn\": true}\n").unwrap();
        assert_eq!(std::fs::read(run_dir.join("events.jsonl")).unwrap(), old_bytes);
        // The stale tmp does not confuse the loader.
        assert_eq!(load_event_log(&run_dir).unwrap().records().len(), 2);

        // The next persist completes atomically: torn tmp is replaced, no
        // torn log is ever visible.
        persist_events(&run_dir, &fix2_log(5)).unwrap();
        assert!(!run_dir.join("events.tmp").exists());
        let loaded = load_event_log(&run_dir).unwrap();
        assert_eq!(loaded.records().len(), 5);
        assert!(loaded.verify().is_ok());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn run_rejects_tampered_prior_event_log() {
        let d = tmpdir("evrun");
        let manifest = manifest_two_tasks(&d);
        let driver = Driver::new(config_for(&d, &manifest)).unwrap();

        // First run: honest seeder, produces a valid events.jsonl.
        let run_dir = d.join("run");
        let seeder = spawn_pass_seeder(run_dir.clone());
        let report = driver.run().unwrap();
        assert_eq!(report.outcome, WaveOutcome::Complete);
        std::fs::write(run_dir.join("DONE"), b"1").unwrap();
        seeder.join().unwrap();
        assert!(run_dir.join("events.jsonl").is_file());

        // Tamper with the persisted log, then run again into the same dir.
        let path = run_dir.join("events.jsonl");
        let mut bytes = std::fs::read(&path).unwrap();
        let mid = bytes.len() / 2;
        bytes[mid] ^= 0x01;
        std::fs::write(&path, &bytes).unwrap();

        let driver2 = Driver::new(config_for(&d, &manifest)).unwrap();
        let err = driver2.run().unwrap_err();
        let msg = format!("{err}");
        assert!(
            matches!(err, DriverError::Record(_)),
            "expected typed Record error, got: {msg}"
        );
        assert!(msg.contains("events.jsonl"), "error must name the file: {msg}");
        let _ = std::fs::remove_dir_all(&d);
    }
}
