#!/usr/bin/env python3
"""PHASE 0: compile the master spec into 150 leaf task contracts.

Emits work-order/manifest.json with the required acceptance fields for every
leaf task, plus a validation report (unique IDs, dependency existence,
cycle detection via Kahn's algorithm, batch sizes, requirement coverage).

T101-T150 objectives were not present in the received spec text (original
sections 3-4 missing). They are preserved as RESERVED obligations with an
explicit block on SPEC-SECTIONS-3-4 rather than invented objectives —
inventing them would violate provenance enforcement.
"""
import hashlib
import json
import os
import sys
from collections import defaultdict, deque

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
SPEC_VERSION = "master-spec v2026-10-09"

# (task_id, section, objective, dependencies-by-interface-contract)
TASKS = [
    # A. Foundation types
    ("T001", "A", "Implement validated, immutable TaskId and SpecId types with stable serialization, ordering, hashing, and display.", []),
    ("T002", "A", "Implement TaskClass enum for PLAN, IMPLEMENT, REVIEW, REPAIR, TEST_GENERATE, ADVERSARIAL, INTEGRATE, VERIFY, RELEASE, and explicit unknown-class handling.", []),
    ("T003", "A", "Implement CapabilityRequirement with capability names, quality tier, context budget, structured-output needs, and optional tool requirements.", []),
    ("T004", "A", "Implement TaskContract containing ID, spec revision, class, objective, inputs, outputs, dependencies, scope, acceptance predicates, verifiers, retry policy, and risk tier.", ["T001", "T002", "T003"]),
    ("T005", "A", "Implement TaskState and validate every allowed state transition; reject illegal transitions.", ["T001"]),
    ("T006", "A", "Implement content-addressed ArtifactRef with digest, media type, size, and provenance.", ["T001"]),
    ("T007", "A", "Implement EvidenceReceipt linking task, attempt, artifact digests, commands, exit codes, verifier, and verdict.", ["T001", "T006", "T008"]),
    ("T008", "A", "Implement Verdict and FailureKind distinguishing PASS, FAIL, INCONCLUSIVE, BLOCKED, INVALID, and INFRA_ERROR.", []),
    ("T009", "A", "Implement ProviderId, ModelId, EndpointId, and credential-reference types; never expose secrets in debug or serialization.", []),
    ("T010", "A", "Implement shared structured errors for policy denial, quota exhaustion, timeout, transport failure, malformed output, conflicts, and verification failure.", []),
    # B. Specification compiler
    ("T011", "B", "Parse a versioned master specification into typed requirements while preserving source text and stable source locations.", ["T001", "T010"]),
    ("T012", "B", "Generate deterministic requirement IDs; detect duplicate IDs, collisions, and unstable source anchors.", ["T011"]),
    ("T013", "B", "Represent acceptance conditions as typed predicates; flag requirements lacking a testable evidence plan.", ["T011"]),
    ("T014", "B", "Build a traceability graph mapping every requirement to tasks, interfaces, tests, and evidence; detect uncovered requirements.", ["T004", "T011", "T012"]),
    ("T015", "B", "Define the planner-model input/output contract for decomposing requirements into tasks; validate every result against the source spec.", ["T004", "T011"]),
    ("T016", "B", "Detect oversized or mixed-responsibility tasks using configurable bounds; request decomposition rather than silently accepting them.", ["T004"]),
    ("T017", "B", "Reject incomplete task contracts missing objective, inputs, outputs, scope, acceptance, verifier, dependencies, or failure path.", ["T004"]),
    ("T018", "B", "Build a dependency DAG, detect cycles, and report a concrete cycle path.", ["T001", "T004", "T010"]),
    ("T019", "B", "Render deterministic one-shot worker prompts from task contracts, relevant requirements, shared contracts, and output schemas.", ["T004", "T011", "T013"]),
    ("T020", "B", "Emit a versioned machine-readable work-order manifest with stable IDs, dependency edges, prompt digests, and coverage statistics.", ["T004", "T014", "T018", "T019"]),
    # C. Capability registry
    ("T021", "C", "Implement a provider-independent capability catalog for coding, reviewing, repairing, test generation, adversarial analysis, and integration.", ["T003"]),
    ("T022", "C", "Register inference endpoints with provider, model, transport, capabilities, context window, structured-output support, and enabled status.", ["T009", "T010", "T021"]),
    ("T023", "C", "Validate endpoint declarations; reject invalid capabilities, impossible context settings, duplicate IDs, and missing mandatory fields.", ["T021", "T022"]),
    ("T024", "C", "Track endpoint health, last success, recent failures, cooldown deadline, and health confidence.", ["T022"]),
    ("T025", "C", "Maintain rolling latency and completion statistics by endpoint and task class.", ["T002", "T022"]),
    ("T026", "C", "Track evidence-based task success rates by endpoint and class; exclude unverified self-reports.", ["T002", "T007", "T022"]),
    ("T027", "C", "Implement deterministic capability matching between task requirements and eligible endpoints.", ["T003", "T021", "T022"]),
    ("T028", "C", "Estimate prompt and expected-output context usage; reject or transform work exceeding endpoint context limits.", ["T003", "T022"]),
    ("T029", "C", "Resolve credentials through secret references and prevent secret leakage into logs, prompts, and receipts.", ["T009"]),
    ("T030", "C", "Expose versioned immutable registry snapshots for reproducible scheduling decisions.", ["T022", "T024", "T025", "T026"]),
    # D. Provider adapters
    ("T031", "D", "Define an async provider-neutral inference trait with typed request, response, cancellation, usage, and error structures.", ["T009", "T039"]),
    ("T032", "D", "Implement configurable HTTP inference transport with timeouts, response-size limits, retry classification, and redacted diagnostics.", ["T010", "T031"]),
    ("T033", "D", "Implement an OpenAI-compatible adapter with configurable endpoint, model, headers, structured-output mode, and usage parsing.", ["T031", "T032"]),
    ("T034", "D", "Implement a configurable NVIDIA inference adapter using documented endpoint settings; do not hardcode undocumented quotas.", ["T031", "T032"]),
    ("T035", "D", "Implement a Gemini adapter with provider-specific request/response mapping and typed quota errors.", ["T031", "T032"]),
    ("T036", "D", "Implement a Copilot integration boundary using supported interfaces only; return explicit unsupported status when headless automation is unavailable.", ["T031"]),
    ("T037", "D", "Implement a local OpenAI-compatible inference adapter with health probing and bounded requests.", ["T031", "T032"]),
    ("T038", "D", "Normalize provider responses into text, structured payload, usage, finish reason, request ID, and response-artifact reference.", ["T006", "T031"]),
    ("T039", "D", "Normalize provider errors into typed retryable/non-retryable errors while preserving safe diagnostic evidence.", ["T010"]),
    ("T040", "D", "Implement a reusable adapter conformance harness for timeout, malformed response, cancellation, usage accounting, and secret redaction.", ["T031", "T032", "T039"]),
    # E. Quota and budget engine
    ("T041", "E", "Model request-rate, token-rate, daily-use, concurrency, cost, and cooldown limits independently; unknown limits remain unknown.", ["T022"]),
    ("T042", "E", "Implement deterministic per-endpoint token-bucket admission control with an injectable clock.", ["T041"]),
    ("T043", "E", "Implement an exact bounded sliding-window request-rate limiter.", ["T041"]),
    ("T044", "E", "Enforce endpoint-specific and global in-flight request limits.", ["T041"]),
    ("T045", "E", "Record provider-reported input/output tokens, requests, latency, errors, and estimated cost where available.", ["T022", "T038"]),
    ("T046", "E", "Model provider quota reset windows and Retry-After data without assuming universal reset behavior.", ["T041"]),
    ("T047", "E", "Reserve request/token/cost budgets before dispatch and reconcile or release reservations after completion or failure.", ["T041", "T042"]),
    ("T048", "E", "Implement configurable weighted fair allocation across task classes to prevent starvation.", ["T041"]),
    ("T049", "E", "Reduce dispatch rate on quota errors or rising latency and recover gradually after successful windows.", ["T041", "T045"]),
    ("T050", "E", "Record why each task was dispatched, delayed, rerouted, or denied due to quota, budget, or fairness policy.", ["T041", "T047", "T048"]),
    # F. Task graph and queue
    ("T051", "F", "Represent task graph nodes and dependency edges with stable IDs and explicit edge semantics.", ["T004", "T018"]),
    ("T052", "F", "Compute ready tasks whose required dependencies have passed their acceptance gates.", ["T004", "T008", "T051"]),
    ("T053", "F", "Implement deterministic task priority from configured risk, critical path, age, class weight, and estimated duration.", ["T004", "T051"]),
    ("T054", "F", "Compute critical-path depth and estimated remaining work using configurable duration estimates.", ["T018", "T051"]),
    ("T055", "F", "Partition ready tasks by class, required capability, context budget, and isolation needs.", ["T003", "T051"]),
    ("T056", "F", "Deduplicate equivalent submissions using explicit idempotency keys and spec revision; preserve tasks with different acceptance criteria.", ["T001", "T004"]),
    ("T057", "F", "Bound queue growth and apply deterministic backpressure.", ["T051"]),
    ("T058", "F", "Propagate cancellation to dependent or superseded tasks according to policy while preserving completed evidence.", ["T007", "T051"]),
    ("T059", "F", "Produce machine-readable reasons and dependency paths for blocked tasks.", ["T051", "T052"]),
    ("T060", "F", "Serialize stable queue snapshots and replay scheduling decisions from recorded inputs.", ["T051", "T052"]),
    # G. Dispatcher and runtime
    ("T061", "G", "Create a dispatch envelope containing task contract, prompt digest, endpoint, limits, workspace, and expected output schema.", ["T004", "T019", "T022", "T028"]),
    ("T062", "G", "Dispatch eligible tasks up to available worker, endpoint, and global concurrency limits.", ["T041", "T044", "T048", "T061"]),
    ("T063", "G", "Enforce connection, first-token, total-response, and task deadlines with typed timeout outcomes.", ["T010", "T061"]),
    ("T064", "G", "Implement bounded retry policies with retry classes, backoff, injectable jitter, and retry budgets.", ["T010", "T061"]),
    ("T065", "G", "Support optional duplicate/hedged inference for explicitly eligible tasks when quota and policy allow it.", ["T047", "T061"]),
    ("T066", "G", "Validate worker output against the required schema before accepting it as a candidate artifact.", ["T004", "T061"]),
    ("T067", "G", "Track in-flight attempt progress without treating worker progress claims as proof.", ["T061"]),
    ("T068", "G", "Recover orphaned RUNNING tasks after restart using leases, attempt IDs, and durable state.", ["T007", "T061", "T084"]),
    ("T069", "G", "Use idempotency keys and effect receipts to prevent duplicate external side effects; explicitly document inference delivery semantics.", ["T056", "T061"]),
    ("T070", "G", "Expose queue wait, service time, retries, errors, utilization, and throughput by task class and endpoint.", ["T045", "T061"]),
    # H. Workspace and Git isolation
    ("T071", "H", "Create isolated per-task Git worktrees or equivalent workspaces pinned to a base commit.", ["T061"]),
    ("T072", "H", "Compare changed paths with each task's allowed scope and reject unauthorized modifications.", ["T004", "T071"]),
    ("T073", "H", "Record base commit, branch, workspace ID, and spec revision for every attempt.", ["T001", "T071"]),
    ("T074", "H", "Capture normalized diffs and changed-file manifests as immutable artifacts before cleanup.", ["T006", "T071"]),
    ("T075", "H", "Hash patches and artifacts and detect post-capture mutation.", ["T006", "T074"]),
    ("T076", "H", "Detect overlapping changed paths and incompatible public-interface changes before integration.", ["T072", "T074"]),
    ("T077", "H", "Build integration workspaces from exact accepted artifact commits and record all parent revisions.", ["T073", "T074"]),
    ("T078", "H", "Remove abandoned worktrees only after preserving required logs, patches, receipts, and recovery metadata.", ["T071", "T074"]),
    ("T079", "H", "Permit local branches and commits under policy; gate pushes, protected merges, tags, releases, and destructive resets.", ["T071"]),
    ("T080", "H", "Record HEAD, dirty state, diff digest, submodule state, and relevant tool versions for each attempt.", ["T071", "T073"]),
    # I. Artifact store and evidence ledger
    ("T081", "I", "Store patches, prompts, responses, logs, test output, manifests, and reports by content digest with type metadata.", ["T006"]),
    ("T082", "I", "Define typed events for compile, schedule, dispatch, response, verify, retry, integrate, deny, release, and failure.", ["T004", "T008"]),
    ("T083", "I", "Add monotonic event sequence numbers and hash chaining; document that hashing detects alteration but does not alone prevent privileged rewriting.", ["T082"]),
    ("T084", "I", "Persist task attempt receipts transactionally so crashes cannot record completion without evidence references.", ["T007", "T081"]),
    ("T085", "I", "Store the exact rendered prompt, task contract version, shared contract version, endpoint identity, and prompt digest.", ["T004", "T019", "T022", "T061"]),
    ("T086", "I", "Store response artifacts or bounded redacted outputs, provider request IDs, usage, finish reasons, and digests.", ["T038", "T081"]),
    ("T087", "I", "Link each acceptance predicate to exact evidence and the verifier version that evaluated it.", ["T007", "T013"]),
    ("T088", "I", "Redact credentials and configured sensitive patterns while preserving reproducibility metadata.", ["T009", "T081"]),
    ("T089", "I", "Query task lineage, failed gates, endpoint performance, requirement coverage, and artifact ancestry.", ["T014", "T082"]),
    ("T090", "I", "Export and restore verifiable ledger bundles, validating hashes, sequence continuity, schema versions, and artifact references.", ["T081", "T083"]),
    # J. Verification engine
    ("T091", "J", "Register deterministic verifier commands and typed verifier capabilities by task class and requirement predicate.", ["T002", "T013"]),
    ("T092", "J", "Execute configured verification commands with bounded time, output, environment, process tree, and resource limits.", ["T008", "T091"]),
    ("T093", "J", "Capture actual exit codes and distinguish test failure from verification-runner infrastructure failure.", ["T008", "T092"]),
    ("T094", "J", "Normalize supported test outputs into pass/fail/skip counts and evidence references without inventing missing results.", ["T092"]),
    ("T095", "J", "Evaluate each acceptance predicate independently; missing evidence yields INCONCLUSIVE or FAIL according to policy, never PASS.", ["T008", "T013"]),
    ("T096", "J", "Generate requirement coverage reports showing proven, failed, blocked, and unverified requirements with evidence links.", ["T014", "T095"]),
    ("T097", "J", "Dispatch independent review to a distinct eligible endpoint; prohibit the implementer's response from serving as its own review evidence.", ["T003", "T022", "T027"]),
    ("T098", "J", "Require review findings to include severity, task/requirement reference, evidence, reproduction steps, and confidence.", ["T004", "T097"]),
    ("T099", "J", "Cache verification results only when source digest, command, environment fingerprint, verifier version, and relevant dependencies match.", ["T006", "T091", "T092"]),
    ("T100", "J", "Produce verdict receipts with predicate-level outcomes, commands, versions, and evidence digests.", ["T007", "T092", "T095"]),
]

SECTION_NAMES = {
    "A": "Foundation types", "B": "Specification compiler",
    "C": "Capability registry", "D": "Provider adapters",
    "E": "Quota and budget engine", "F": "Task graph and queue",
    "G": "Dispatcher and runtime", "H": "Workspace and Git isolation",
    "I": "Artifact store and evidence ledger", "J": "Verification engine",
}

CRATE_FOR_SECTION = {
    "A": "crates/fabric-types", "B": "crates/fabric-spec",
    "C": "crates/fabric-registry", "D": "crates/fabric-adapters",
    "E": "crates/fabric-quota", "F": "crates/fabric-graph",
    "G": "crates/fabric-dispatch", "H": "crates/fabric-workspace",
    "I": "crates/fabric-ledger", "J": "crates/fabric-verify",
}

BATCHES = {"batch-1": (1, 39), "batch-2": (40, 78), "batch-3": (79, 117), "batch-4": (118, 150)}


def batch_of(n):
    for name, (lo, hi) in BATCHES.items():
        if lo <= n <= hi:
            return name
    raise ValueError(n)


def main():
    contracts = []
    for tid, section, objective, deps in TASKS:
        n = int(tid[1:])
        prompt_digest = hashlib.sha256(objective.encode()).hexdigest()
        contracts.append({
            "stable_id": tid,
            "versioned_spec_reference": f"{SPEC_VERSION} §2.{section}",
            "objective": objective,
            "allowed_scope": [CRATE_FOR_SECTION[section]],
            "inputs": [f"{d}.output" for d in deps] or ["master-spec"],
            "outputs": [f"{tid}.implementation", f"{tid}.tests", f"{tid}.receipt"],
            "dependencies": sorted(deps),
            "acceptance_predicates": [
                f"{tid}.objective-implemented",
                f"{tid}.unit-tests-pass",
                f"{tid}.no-secret-leakage",
                f"{tid}.interface-contract-stable",
            ],
            "verifier_ids": ["cargo-test", "cargo-clippy", "interface-diff"],
            "failure_state": "FAIL",
            "evidence_references": [],
            "prompt_digest": f"sha256:{prompt_digest}",
            "batch": batch_of(n),
            "section": section,
            "status": "specified",
        })
    # T101-T150: obligations preserved, objectives unknown (spec sections 3-4 missing)
    for n in range(101, 151):
        tid = f"T{n:03d}"
        contracts.append({
            "stable_id": tid,
            "versioned_spec_reference": f"{SPEC_VERSION} §3-4 (NOT RECEIVED)",
            "objective": "RESERVED: original specification sections 3-4 not received; obligation preserved, decomposition blocked pending spec. Do not invent an objective.",
            "allowed_scope": [],
            "inputs": ["SPEC-SECTIONS-3-4"],
            "outputs": [],
            "dependencies": ["SPEC-SECTIONS-3-4"],
            "acceptance_predicates": [],
            "verifier_ids": [],
            "failure_state": "BLOCKED",
            "evidence_references": [],
            "prompt_digest": None,
            "batch": batch_of(n),
            "section": "RESERVED",
            "status": "blocked-on-spec",
        })

    # ---- validation ----
    errors = []
    ids = [c["stable_id"] for c in contracts]
    if len(ids) != 150:
        errors.append(f"expected 150 contracts, got {len(ids)}")
    if len(set(ids)) != len(ids):
        errors.append("duplicate stable IDs")
    idset = set(ids)
    for c in contracts:
        for d in c["dependencies"]:
            if d not in idset and d != "SPEC-SECTIONS-3-4":
                errors.append(f"{c['stable_id']}: unknown dependency {d}")
    # cycle detection (Kahn) over specified tasks only
    adj = defaultdict(list)
    indeg = defaultdict(int)
    spec_ids = [c["stable_id"] for c in contracts if c["status"] == "specified"]
    for c in contracts:
        if c["status"] != "specified":
            continue
        for d in c["dependencies"]:
            adj[d].append(c["stable_id"])
            indeg[c["stable_id"]] += 1
    q = deque([i for i in spec_ids if indeg[i] == 0])
    order = []
    while q:
        u = q.popleft()
        order.append(u)
        for v in adj[u]:
            indeg[v] -= 1
            if indeg[v] == 0:
                q.append(v)
    if len(order) != len(spec_ids):
        cyc = [i for i in spec_ids if indeg[i] > 0]
        errors.append(f"dependency cycle involving: {cyc}")
    # batch sizes
    for name, (lo, hi) in BATCHES.items():
        cnt = sum(1 for c in contracts if c["batch"] == name)
        if cnt != (hi - lo + 1):
            errors.append(f"{name}: expected {hi-lo+1}, got {cnt}")
    # coverage: every received spec section A-J has >=1 task
    for s in "ABCDEFGHIJ":
        if not any(c["section"] == s for c in contracts):
            errors.append(f"spec section {s} uncovered")

    manifest = {
        "manifest_version": "1",
        "spec_version": SPEC_VERSION,
        "generated_by": "tools/gen_manifest.py (PHASE 0)",
        "leaf_tasks": contracts,
        "integration_batches": {k: {"range": list(v), "size": v[1] - v[0] + 1} for k, v in BATCHES.items()},
        "risks": [
            "policy-undefined: T058/T064/T065/T079/T095 and acceptance invariants reference an authorization policy, but no T001-T100 task defines policy semantics. If reserved spec sections 3-4 lack a policy task, it is silently dropped.",
            "T068 cross-batch: durable-state recovery depends on T084 (batch 3); T068 cannot pass its gate before I3."
        ],
        "validation": {
            "passed": not errors,
            "errors": errors,
            "topological_order": order,
            "reserved_blocked": [c["stable_id"] for c in contracts if c["status"] == "blocked-on-spec"],
        },
    }
    out = os.path.join(ROOT, "work-order", "manifest.json")
    with open(out, "w") as f:
        json.dump(manifest, f, indent=2)
    print(f"wrote {out}: {len(contracts)} contracts")
    print(f"validation passed: {not errors}")
    for e in errors:
        print(f"  ERROR: {e}")
    if errors:
        sys.exit(1)
    # coverage stats
    cov = defaultdict(int)
    for c in contracts:
        if c["status"] == "specified":
            cov[c["section"]] += 1
    print("coverage:", dict(cov))


if __name__ == "__main__":
    main()
