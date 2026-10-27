# Work-order lifecycle contracts

Compiled from the SaaS systems critique §4 (2026-10-10) into standing architecture.
Companion to WORKER-PROTOCOL.md (cell side); this is the orchestrator/governor side.

## The five contracts

| Contract | Required invariant |
|---|---|
| Task | Objective, scope, dependencies, acceptance criteria, explicit failure conditions. No task dispatches without all five. |
| Assignment | One owner, one bounded worktree, one immutable starting revision. The revision is pinned; the worker may not move it. |
| Deliverable | Patch + machine-readable report + reproducible commands. Narrative alone is not a deliverable. |
| Verification | An independent verifier checks the patch against the contract. The verifier is never the worker, never the planner. |
| Integration | Only a policy-controlled integrator merges or promotes accepted work. Workers cannot self-promote. |

Composition is recursive: an organelle exposes a contract; its internals decompose
into child organelles; the parent verifies child interfaces and integrates. The
planner may be wrong — it is never trusted to certify its own output.

## Deterministic lifecycle

```
QUEUED → ASSIGNED → EXECUTING → VERIFYING → ACCEPTED
```

Terminal/recovery states: `BLOCKED`, `REJECTED`, `CANCELLED`.

### Transition guards (evidence-gated, no exceptions)

- `QUEUED → ASSIGNED`: task record complete (all five Task fields), owner exists,
  worktree allocated, starting revision pinned and recorded.
- `ASSIGNED → EXECUTING`: worker acknowledged receipt of brief + revision pin.
  Acknowledgment is a signed receipt, not a chat message.
- `EXECUTING → VERIFYING`: deliverable present — patch, machine-readable report,
  reproducible commands. Missing any one = stays EXECUTING (or BLOCKED on timeout).
- `VERIFYING → ACCEPTED`: patch tested against the pinned revision and the
  acceptance contract by the independent verifier; regression/negative tests
  demonstrate vulnerable-vs-fixed distinction where applicable; evidence bound
  (revision, patch digest, commands, exit codes, environment, raw output,
  verifier identity). **A timeout is not success. A missing result does not
  disprove failure. "Done" from the worker does not close the task.**
- `* → BLOCKED`: guard evidence shows the task cannot proceed (missing dep,
  broken toolchain, ambiguous contract). Records the blocking cause.
- `VERIFYING → REJECTED`: verifier found contract violation, bypass, or
  incomplete evidence. Records the defeating evidence.
- `* → CANCELLED`: governor decision with recorded reason. Cancellation is a
  decision, not a disappearance.

Trinary discipline applies to the engine itself: every task is PROVEN (accepted
with bound evidence), DISPROVEN (rejected with defeating evidence), or UNKNOWN
(everything else). There is no "probably done."

## Idempotency and side effects

- Stable task identity: `task_id` is immutable across retries.
- Every execution is an **attempt**: `attempt_id` increments per run.
- Re-running a task must not silently duplicate side effects. Where retries
  could spend money, mutate production data, or deploy code: idempotency keys
  or explicit compensation, recorded in the task record before EXECUTING.
- Cancellation semantics: a cancelled attempt's partial side effects are
  enumerated in the cancellation record; unknown side effects keep the task
  UNKNOWN, never ACCEPTED.

## Notion notes (standing practice, Zack 2026-10-10)

- `VERIFYING → ACCEPTED`: the integrator appends a dated entry to the Notion
  Work Order Log (`3f698a9c-4d31-8154-a3ec-fddbe0d84c84`): task ID, objective,
  verdict, evidence pointer. No work order closes silently.
- Every planning phase gets its own dated note.
- Conversations that decide things are meetings: the agent writes meeting
  notes to the Notion Meeting Notes page (`3f698a9c-4d31-8116-81ec-dd5e9c6260fb`).
- Targeted appends, never full rewrites.

## What this kills

- Worker self-certification ("done" closing tasks).
- Timeout-as-success.
- Missing-result-as-disproof.
- Unbounded retry side effects (double-spend, double-deploy).
- Planner-certifies-own-output.

Status: CONTRACT (not advisory). The dispatcher enforces it; waves that violate
it fail governance.
