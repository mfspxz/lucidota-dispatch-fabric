# MASTER SPEC — Agent Workforce Dispatch System

**Source:** Zackary Cameron, 2026-10-09 (side chat, verbatim work order).
**Status:** INVARIANT SOURCE. Every audit round (RT1–RT3, DS1–DS3) uses this document plus the exact candidate commit. Never a remembered version.
**Version:** `master-spec v2026-10-09`
**Provenance note:** Sections are numbered as received (§2, then §5–§9). Original §§3–§4 and leaf tasks T101–T150 were not included in the received text; those obligations are preserved as RESERVED in the work-order manifest, decomposition blocked pending the missing sections.

---

Zack's order: "Maximize workers on this. And try to do it properly the way it will be done when it's built."

## 2. The 150 leaf prompts

Each entry below is a separate task prompt. Its `OBJECTIVE` is combined with the universal leaf-task prompt above. Dependencies are determined by interface contracts and the compiled DAG; section order is not an instruction to execute sequentially.

### A. Foundation types — T001–T010

```yaml
- id: T001
  objective: Implement validated, immutable TaskId and SpecId types with stable serialization, ordering, hashing, and display.
- id: T002
  objective: Implement TaskClass enum for PLAN, IMPLEMENT, REVIEW, REPAIR, TEST_GENERATE, ADVERSARIAL, INTEGRATE, VERIFY, RELEASE, and explicit unknown-class handling.
- id: T003
  objective: Implement CapabilityRequirement with capability names, quality tier, context budget, structured-output needs, and optional tool requirements.
- id: T004
  objective: Implement TaskContract containing ID, spec revision, class, objective, inputs, outputs, dependencies, scope, acceptance predicates, verifiers, retry policy, and risk tier.
- id: T005
  objective: Implement TaskState and validate every allowed state transition; reject illegal transitions.
- id: T006
  objective: Implement content-addressed ArtifactRef with digest, media type, size, and provenance.
- id: T007
  objective: Implement EvidenceReceipt linking task, attempt, artifact digests, commands, exit codes, verifier, and verdict.
- id: T008
  objective: Implement Verdict and FailureKind distinguishing PASS, FAIL, INCONCLUSIVE, BLOCKED, INVALID, and INFRA_ERROR.
- id: T009
  objective: Implement ProviderId, ModelId, EndpointId, and credential-reference types; never expose secrets in debug or serialization.
- id: T010
  objective: Implement shared structured errors for policy denial, quota exhaustion, timeout, transport failure, malformed output, conflicts, and verification failure.
```

### B. Specification compiler — T011–T020

```yaml
- id: T011
  objective: Parse a versioned master specification into typed requirements while preserving source text and stable source locations.
- id: T012
  objective: Generate deterministic requirement IDs; detect duplicate IDs, collisions, and unstable source anchors.
- id: T013
  objective: Represent acceptance conditions as typed predicates; flag requirements lacking a testable evidence plan.
- id: T014
  objective: Build a traceability graph mapping every requirement to tasks, interfaces, tests, and evidence; detect uncovered requirements.
- id: T015
  objective: Define the planner-model input/output contract for decomposing requirements into tasks; validate every result against the source spec.
- id: T016
  objective: Detect oversized or mixed-responsibility tasks using configurable bounds; request decomposition rather than silently accepting them.
- id: T017
  objective: Reject incomplete task contracts missing objective, inputs, outputs, scope, acceptance, verifier, dependencies, or failure path.
- id: T018
  objective: Build a dependency DAG, detect cycles, and report a concrete cycle path.
- id: T019
  objective: Render deterministic one-shot worker prompts from task contracts, relevant requirements, shared contracts, and output schemas.
- id: T020
  objective: Emit a versioned machine-readable work-order manifest with stable IDs, dependency edges, prompt digests, and coverage statistics.
```

### C. Capability registry — T021–T030

```yaml
- id: T021
  objective: Implement a provider-independent capability catalog for coding, reviewing, repairing, test generation, adversarial analysis, and integration.
- id: T022
  objective: Register inference endpoints with provider, model, transport, capabilities, context window, structured-output support, and enabled status.
- id: T023
  objective: Validate endpoint declarations; reject invalid capabilities, impossible context settings, duplicate IDs, and missing mandatory fields.
- id: T024
  objective: Track endpoint health, last success, recent failures, cooldown deadline, and health confidence.
- id: T025
  objective: Maintain rolling latency and completion statistics by endpoint and task class.
- id: T026
  objective: Track evidence-based task success rates by endpoint and class; exclude unverified self-reports.
- id: T027
  objective: Implement deterministic capability matching between task requirements and eligible endpoints.
- id: T028
  objective: Estimate prompt and expected-output context usage; reject or transform work exceeding endpoint context limits.
- id: T029
  objective: Resolve credentials through secret references and prevent secret leakage into logs, prompts, and receipts.
- id: T030
  objective: Expose versioned immutable registry snapshots for reproducible scheduling decisions.
```

### D. Provider adapters — T031–T040

```yaml
- id: T031
  objective: Define an async provider-neutral inference trait with typed request, response, cancellation, usage, and error structures.
- id: T032
  objective: Implement configurable HTTP inference transport with timeouts, response-size limits, retry classification, and redacted diagnostics.
- id: T033
  objective: Implement an OpenAI-compatible adapter with configurable endpoint, model, headers, structured-output mode, and usage parsing.
- id: T034
  objective: Implement a configurable NVIDIA inference adapter using documented endpoint settings; do not hardcode undocumented quotas.
- id: T035
  objective: Implement a Gemini adapter with provider-specific request/response mapping and typed quota errors.
- id: T036
  objective: Implement a Copilot integration boundary using supported interfaces only; return explicit unsupported status when headless automation is unavailable.
- id: T037
  objective: Implement a local OpenAI-compatible inference adapter with health probing and bounded requests.
- id: T038
  objective: Normalize provider responses into text, structured payload, usage, finish reason, request ID, and response-artifact reference.
- id: T039
  objective: Normalize provider errors into typed retryable/non-retryable errors while preserving safe diagnostic evidence.
- id: T040
  objective: Implement a reusable adapter conformance harness for timeout, malformed response, cancellation, usage accounting, and secret redaction.
```

Batch 1 integration boundary: T001–T039. T040 belongs to batch 2, so the adapter conformance harness is not a prerequisite for compiling batch 1; it becomes a required gate when available.
### E. Quota and budget engine — T041–T050

```yaml
- id: T041
  objective: Model request-rate, token-rate, daily-use, concurrency, cost, and cooldown limits independently; unknown limits remain unknown.
- id: T042
  objective: Implement deterministic per-endpoint token-bucket admission control with an injectable clock.
- id: T043
  objective: Implement an exact bounded sliding-window request-rate limiter.
- id: T044
  objective: Enforce endpoint-specific and global in-flight request limits.
- id: T045
  objective: Record provider-reported input/output tokens, requests, latency, errors, and estimated cost where available.
- id: T046
  objective: Model provider quota reset windows and Retry-After data without assuming universal reset behavior.
- id: T047
  objective: Reserve request/token/cost budgets before dispatch and reconcile or release reservations after completion or failure.
- id: T048
  objective: Implement configurable weighted fair allocation across task classes to prevent starvation.
- id: T049
  objective: Reduce dispatch rate on quota errors or rising latency and recover gradually after successful windows.
- id: T050
  objective: Record why each task was dispatched, delayed, rerouted, or denied due to quota, budget, or fairness policy.
```

### F. Task graph and queue — T051–T060

```yaml
- id: T051
  objective: Represent task graph nodes and dependency edges with stable IDs and explicit edge semantics.
- id: T052
  objective: Compute ready tasks whose required dependencies have passed their acceptance gates.
- id: T053
  objective: Implement deterministic task priority from configured risk, critical path, age, class weight, and estimated duration.
- id: T054
  objective: Compute critical-path depth and estimated remaining work using configurable duration estimates.
- id: T055
  objective: Partition ready tasks by class, required capability, context budget, and isolation needs.
- id: T056
  objective: Deduplicate equivalent submissions using explicit idempotency keys and spec revision; preserve tasks with different acceptance criteria.
- id: T057
  objective: Bound queue growth and apply deterministic backpressure.
- id: T058
  objective: Propagate cancellation to dependent or superseded tasks according to policy while preserving completed evidence.
- id: T059
  objective: Produce machine-readable reasons and dependency paths for blocked tasks.
- id: T060
  objective: Serialize stable queue snapshots and replay scheduling decisions from recorded inputs.
```

### G. Dispatcher and runtime — T061–T070

```yaml
- id: T061
  objective: Create a dispatch envelope containing task contract, prompt digest, endpoint, limits, workspace, and expected output schema.
- id: T062
  objective: Dispatch eligible tasks up to available worker, endpoint, and global concurrency limits.
- id: T063
  objective: Enforce connection, first-token, total-response, and task deadlines with typed timeout outcomes.
- id: T064
  objective: Implement bounded retry policies with retry classes, backoff, injectable jitter, and retry budgets.
- id: T065
  objective: Support optional duplicate/hedged inference for explicitly eligible tasks when quota and policy allow it.
- id: T066
  objective: Validate worker output against the required schema before accepting it as a candidate artifact.
- id: T067
  objective: Track in-flight attempt progress without treating worker progress claims as proof.
- id: T068
  objective: Recover orphaned RUNNING tasks after restart using leases, attempt IDs, and durable state.
- id: T069
  objective: Use idempotency keys and effect receipts to prevent duplicate external side effects; explicitly document inference delivery semantics.
- id: T070
  objective: Expose queue wait, service time, retries, errors, utilization, and throughput by task class and endpoint.
```

### H. Workspace and Git isolation — T071–T080

```yaml
- id: T071
  objective: Create isolated per-task Git worktrees or equivalent workspaces pinned to a base commit.
- id: T072
  objective: Compare changed paths with each task's allowed scope and reject unauthorized modifications.
- id: T073
  objective: Record base commit, branch, workspace ID, and spec revision for every attempt.
- id: T074
  objective: Capture normalized diffs and changed-file manifests as immutable artifacts before cleanup.
- id: T075
  objective: Hash patches and artifacts and detect post-capture mutation.
- id: T076
  objective: Detect overlapping changed paths and incompatible public-interface changes before integration.
- id: T077
  objective: Build integration workspaces from exact accepted artifact commits and record all parent revisions.
- id: T078
  objective: Remove abandoned worktrees only after preserving required logs, patches, receipts, and recovery metadata.
- id: T079
  objective: Permit local branches and commits under policy; gate pushes, protected merges, tags, releases, and destructive resets.
- id: T080
  objective: Record HEAD, dirty state, diff digest, submodule state, and relevant tool versions for each attempt.
```

### I. Artifact store and evidence ledger — T081–T090

```yaml
- id: T081
  objective: Store patches, prompts, responses, logs, test output, manifests, and reports by content digest with type metadata.
- id: T082
  objective: Define typed events for compile, schedule, dispatch, response, verify, retry, integrate, deny, release, and failure.
- id: T083
  objective: Add monotonic event sequence numbers and hash chaining; document that hashing detects alteration but does not alone prevent privileged rewriting.
- id: T084
  objective: Persist task attempt receipts transactionally so crashes cannot record completion without evidence references.
- id: T085
  objective: Store the exact rendered prompt, task contract version, shared contract version, endpoint identity, and prompt digest.
- id: T086
  objective: Store response artifacts or bounded redacted outputs, provider request IDs, usage, finish reasons, and digests.
- id: T087
  objective: Link each acceptance predicate to exact evidence and the verifier version that evaluated it.
- id: T088
  objective: Redact credentials and configured sensitive patterns while preserving reproducibility metadata.
- id: T089
  objective: Query task lineage, failed gates, endpoint performance, requirement coverage, and artifact ancestry.
- id: T090
  objective: Export and restore verifiable ledger bundles, validating hashes, sequence continuity, schema versions, and artifact references.
```

### J. Verification engine — T091–T100

```yaml
- id: T091
  objective: Register deterministic verifier commands and typed verifier capabilities by task class and requirement predicate.
- id: T092
  objective: Execute configured verification commands with bounded time, output, environment, process tree, and resource limits.
- id: T093
  objective: Capture actual exit codes and distinguish test failure from verification-runner infrastructure failure.
- id: T094
  objective: Normalize supported test outputs into pass/fail/skip counts and evidence references without inventing missing results.
- id: T095
  objective: Evaluate each acceptance predicate independently; missing evidence yields INCONCLUSIVE or FAIL according to policy, never PASS.
- id: T096
  objective: Generate requirement coverage reports showing proven, failed, blocked, and unverified requirements with evidence links.
- id: T097
  objective: Dispatch independent review to a distinct eligible endpoint; prohibit the implementer's response from serving as its own review evidence.
- id: T098
  objective: Require review findings to include severity, task/requirement reference, evidence, reproduction steps, and confidence.
- id: T099
  objective: Cache verification results only when source digest, command, environment fingerprint, verifier version, and relevant dependencies match.
- id: T100
  objective: Produce verdict receipts with predicate-level outcomes, commands, versions, and evidence digests.
```

---

## 5. Adversarial audit pipeline

Use the original specification as the invariant source in every round. The audit target is the exact candidate commit, not a remembered version of the code.

The control framework draws on three established references:

* [OWASP Top 10 for Agentic Applications 2026](https://genai.owasp.org/resource/owasp-top-10-for-agentic-applications-for-2026/) — agentic risks and attack surfaces.
* [Google Secure AI Framework — controls](https://www.saif.google/secure-ai-framework/controls) — input/output validation, least privilege, agent observability, red teaming, vulnerability management, and incident response.
* [NIST AI RMF Generative AI Profile](https://nvlpubs.nist.gov/nistpubs/ai/NIST.AI.600-1.pdf) — lifecycle-oriented risk management and evaluation.

These are threat-model and control references, not certifications. The audit workers must produce reproducible findings; a checklist marked complete is not sufficient evidence.

### RT1 — Specification conformance

```text
AUDIT_ID: RT1
TARGET: original specification + all 150 task contracts + exact candidate diff

ATTACK OBJECTIVES:
- Missing requirements.
- Incorrectly decomposed requirements.
- Acceptance criteria weakened or silently removed.
- Dependency unlocks before prerequisite acceptance.
- Interface/schema incompatibility.
- Missing or forged evidence.
- Worker claims treated as proof.
- Integration drops a task or accepted artifact.

OUTPUT:
finding_id, severity, affected_requirement_ids, affected_task_ids,
source_location, evidence, reproduction_steps, expected_behavior,
observed_behavior, proposed_regression_test, disposition.

Confirm findings with deterministic checks where possible.
Do not modify the candidate while auditing.
```

### RT2 — Authority and security boundaries

```text
AUDIT_ID: RT2
TARGET: exact candidate commit; policy, dispatch, workspace, ledger, release

ATTACK OBJECTIVES:
- Prompt injection in repository artifacts or model responses.
- Unauthorized filesystem writes.
- Policy or verifier modification by workers.
- Self-approval or forged receipts.
- Credential leakage through prompts, logs or errors.
- Unauthorized network/tool actions.
- Protected-branch or release authorization bypass.
- Replay and idempotency failures.
- Queue, context, response-size or resource exhaustion.
- Secret access outside task scope.

METHOD:
Use isolated fixtures, synthetic secrets, fake providers, bounded workloads,
and safe local adversarial tests. Do not run destructive tests against live
accounts, production systems, or unrelated repositories.

OUTPUT:
Evidence-linked findings, reproducible exploit/test, severity,
affected requirements, mitigation hypothesis, and regression-test proposal.
```

### RT3 — Reliability and integration attacks

```text
AUDIT_ID: RT3
TARGET: exact root candidate and all four subsystem manifests

ATTACK OBJECTIVES:
- Crash at each durable state transition.
- Duplicate dispatch after restart.
- Partial or malformed provider responses.
- Stale verification cache.
- Wrong base commit or mismatched artifact digest.
- Conflicting patches and incompatible interfaces.
- Lost, reordered, or duplicated durable events.
- Nondeterministic scheduling for identical recorded inputs.
- Rollback corrupting accepted artifacts or evidence.
- Test flakiness and false-positive verification.
- Repair loops that never terminate or exhaust budgets.
- Activation despite unresolved mandatory failures.

METHOD:
Use fault injection, deterministic fake clocks/endpoints, randomized DAGs,
crash/recovery tests, and reproducible regression cases.

OUTPUT:
Evidence-linked findings, exact reproduction, impacted state transitions,
required repair, and regression tests.
```

## 6. Independent DeepSeek passes

These are separate review calls, not additional implementation workers. Use the original specification and the exact candidate revision in every pass.

```yaml
- id: DS1
  target: root_candidate + original_spec + traceability_matrix
  focus:
    - semantic_requirement_gaps
    - architectural_contradictions
    - missing_edge_cases
    - unnecessary_complexity
  rule: findings must identify concrete code/contracts and falsifiable conditions

- id: DS2
  target: security_boundary + RT1_RT2_RT3_findings
  focus:
    - authorization_bypass
    - prompt_injection
    - unsafe_defaults
    - secrets_and_data_exfiltration
    - supply_chain_risks
  rule: inspect changed code; confirm or reject findings with evidence

- id: DS3
  target: post_repair_candidate + regression_results
  focus:
    - regressions_introduced_by_fixes
    - uncovered_requirements
    - incorrectly_closed_findings
    - release_gate_bypass
  rule: re-test the exact new commit; no finding closes without evidence
```

## 7. Full execution recurrence

```text
PHASE 0:
  Inspect repository.
  Compile master spec.
  Generate exactly 150 leaf task contracts.
  Validate requirement coverage, dependency DAG, scopes, and prompt schemas.
  Reject silent requirement loss.

PHASE 1:
  Dispatch T001-T039 to eligible endpoints.
  Isolate every task workspace.
  Capture patches and evidence.
  Run configured deterministic verification.
  Integrate with I1.

PHASE 2:
  Dispatch T040-T078 as dependencies and capacity permit.
  Capture patches and evidence.
  Run configured deterministic verification.
  Integrate with I2.

PHASE 3:
  Dispatch T079-T117 as dependencies and capacity permit.
  Capture patches and evidence.
  Run configured deterministic verification.
  Integrate with I3.

PHASE 4:
  Dispatch T118-T150 as dependencies and capacity permit.
  Capture patches and evidence.
  Run configured deterministic verification.
  Integrate with I4.

PHASE 5:
  Run ROOT integration.
  Run root deterministic acceptance suite.
  Require complete mandatory requirement coverage.

PHASE 6:
  RT1 -> repair -> deterministic regression tests.
  RT2 -> repair -> deterministic regression tests.
  RT3 -> repair -> deterministic regression tests.
  Repeat affected audits whenever changes invalidate prior evidence.

PHASE 7:
  DS1 -> repair -> deterministic regression tests.
  DS2 -> repair -> deterministic regression tests.
  DS3 -> repair -> full deterministic regression tests.
  Repeat affected reviews whenever changes invalidate prior evidence.

PHASE 8:
  Re-run the complete mandatory root suite against the exact final candidate.
  Verify all mandatory requirements and unresolved findings.
  Governor/Court decides acceptance.
  Emit ACTIVATION_READY only after acceptance.
  Muse consumes the event and candidate digest.
```

## 8. Scheduling equation

Let be ready tasks of class , available workers from service , and the remaining service capacity. The scheduler maximizes eligible task assignments:

\\max \\sum_{i,s} x_{i,s}

subject to:

\\begin{aligned}
x_{i,s} &\\in \\{0,1\\}\\\\
x_{i,s} &= 0 && \\text{if capability or policy is incompatible}\\\\
\\sum_s x_{i,s} &\\leq 1 && \\forall i\\\\
\\sum_i x_{i,s} &\\leq W_s && \\forall s\\\\
\\sum_i x_{i,s} &\\leq R_s && \\forall s
\\end{aligned}

Only tasks whose required predecessors have passed their gates enter the ready set. Provider quotas are measured in the actual dimensions each provider enforces; 39 RPM does not imply unlimited tokens, concurrency, or successful completions.

## 9. Final acceptance invariants

```yaml
acceptance:
  leaf_tasks:
    total: 150
    required_fields:
      - stable_id
      - versioned_spec_reference
      - objective
      - allowed_scope
      - inputs
      - outputs
      - dependencies
      - acceptance_predicates
      - verifier_ids
      - failure_state
      - evidence_references

  integration:
    batch_sizes: [39, 39, 39, 33]
    batch_artifacts_require:
      - accepted_child_receipts
      - interface_manifest
      - exact_parent_revisions
      - test_receipts
      - requirement_coverage
      - candidate_digest

  root:
    requires:
      - all_mandatory_requirements_proven
      - all_required_subsystem_gates_passed
      - root_deterministic_suite_passed
      - no_unresolved_mandatory_blockers
      - exact_candidate_digest_recorded
      - policy_digest_recorded
      - complete_provenance_report

  activation:
    event: ACTIVATION_READY
    emitted_only_by: governor_court
    requires_root_acceptance: true
    includes_candidate_digest: true
    worker_can_emit_authoritative_event: false

  external_effects:
    push: policy_authorized
    protected_merge: policy_authorized
    release: policy_authorized
    deployment: policy_authorized
```

Important boundary: this is the complete text work order and prompt inventory, not a claim that all 150 tasks are already perfectly decomposed for an unknown repository. The initial high-capability planning call must map these task IDs to the actual codebase, resolve existing interfaces, identify genuine dependencies, and preserve all 150 obligations unless an explicit, recorded decision changes the manifest. That is how the system avoids generating 150 beautifully formatted prompts that build the wrong thing.

That's how we solve this problem
