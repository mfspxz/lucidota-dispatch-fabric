# COMPARTMENTALIZATION DOCTRINE

**Standing order — Zack, 2026-10-09.** Source briefs: `evidence/research/`
(intel tradecraft, clandestine cell structures, RBAC/Sandhu).

## Thesis

Build granular bits. No worker knows the project until it is organelle Lego
at integration. What each role may know is a *designed property* of the
system, not an accident of prompt writing.

Epistemic rule: a worker's prompt must not allow reconstructing the project —
not just withholding the name, but phrasing the task so the operation itself
is concealed from the participant (NATO *clandestine*, not merely *covert*).

## Clearance levels (RBAC)

| Role | Sees | Never sees |
|---|---|---|
| PLANNER / GOVERNOR | whole spec, DAG, integration plan | — (full clearance) |
| INTEGRATOR | accepted artifacts + frozen interfaces | worker internals, prompts |
| WORKER | its task contract only | project name, siblings, DAG, provenance of inputs |
| VERIFIER | task contract + evidence artifacts | worker identity beyond the attempt record |
| AUDITOR | everything, read-only | — (emits findings, changes nothing) |

Workers are never assigned permissions directly — only through the `worker`
role, scoped to one task contract, bound to one attempt *session*.

## Mechanisms

1. **Need-to-know briefs (T019).** Prompts render from the task contract +
   shared contracts only. Never from master-spec text. Opaque IDs
   (`T042`, digests), never project vocabulary.
2. **Cut-out dispatcher (T061).** The dispatcher is the sole holder of the
   task graph. Workers never learn upstream/downstream task IDs or who
   consumes their output. No lateral worker-to-worker channels exist.
3. **Dead-drop artifact store (T081/T084).** Workers write content-addressed
   artifacts; the integrator collects by digest. Structured in/out schemas
   are the dead drop — no free-form coordination channels.
4. **One-way information flow.** All cross-task linking (traceability,
   integration, coverage) happens inside the governor. When a task needs a
   prior artifact, the governor embeds only the artifact bytes, stripped of
   provenance metadata (which worker/task produced it, what it served).
5. **Burn containment (T071).** One isolated workspace per task. A bad or
   compromised worker's blast radius is its own tree: quarantine its
   artifacts, rotate the cell slot, never let its context bleed into the
   next dispatch.
6. **Static separation of duties.** `worker ≠ verifier` for the same task is
   a scheduler invariant, not a policy suggestion — the dispatcher cannot
   construct a violating assignment. Likewise no session activates both
   `worker` and `integrator`.
7. **Session-scoped activation.** Permissions bind to the attempt session,
   not the agent identity. A worker's session token activates only `worker`
   on its task — the token gates the filesystem and the ledger, not the
   prompt text.
8. **Redacted feedback (T088/T095).** Verifier error messages returned to a
   worker must not leak sibling-task information. Missing evidence is
   INCONCLUSIVE/FAIL, never an invitation to go look around.

## Honest boundary

Blindness is role-relative, not absolute. The planner that decomposes the
spec and the governor that integrates necessarily hold full clearance.
The doctrine does not pretend otherwise — it formalizes who sees what, and
makes violations structurally impossible rather than merely forbidden.

## Enforcement, now vs. in the system

- **This build (brief discipline):** cell briefs contain only the task slice;
  isolated git worktrees; file-scope limits; receipt-only reporting upward.
- **The system (mechanical):** T019 redacting renderer, T061 minimal
  envelopes, T072 scope checks, T088 ledger redaction, T097 independent
  review routing, scheduler SSD invariants. The moment any of these is a
  suggestion instead of an invariant, the doctrine is broken.
