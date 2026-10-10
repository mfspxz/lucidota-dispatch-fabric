# Dogfood report — fabric-driver self-hosting run 1

Date: 2026-10-10. Driver: `fabric-driver` examples/run. Manifest: 3-task chain (DOG-A → DOG-B → DOG-C).

## Result: Complete, 3 waves, exit 0

The loop works end to end: manifest → DAG → ready set → isolated workspace →
dispatch envelope → outbox → (external worker) → inbox receipt → shape/linkage
check → hash-chained event log → gate pass → next wave. Report in `run1.log`.

## What the dogfood caught

1. **Fail-closed contract validation (4 rejections before start).** The driver
   refused the synthetic manifest four times, once per missing field: empty
   `allowed_scope`, missing `inputs`, missing `acceptance`, missing
   `verifiers`. Each rejection named the task and the field. The driver will
   not run a malformed work order. This is the design working.
2. **Real driver bug: nested worktree paths.** `git -C <repo> worktree add`
   with a relative path resolves against the repo dir, not the process cwd —
   the first run nested worktrees inside `.driver-repo` itself. Fixed by
   canonicalizing repo and worktree-root to absolute paths in
   `ensure_workspace_repo`. Driver tests (13) still green after the fix.
3. **Trust-boundary proof (the honest part).** The operator playing worker
   made a real error on DOG-B: picked the wrong envelope file from the outbox
   and did DOG-B's work in DOG-A's workspace, then dropped a receipt with the
   right shape and digest linkage. The driver accepted it and marked DOG-B
   passed — because the driver checks receipt *shape and digest linkage only*.
   Deep verification of the work itself is fabric-verify's job, not the
   driver's. The dogfood proved exactly why the verifier layer exists and why
   the driver must never be the verifier. Final workspace state: DOG-A-a1
   held one.txt (+ a stray three.txt from the worker error), DOG-B-a1 was
   empty, DOG-C-a1 held three.txt. The driver reported Complete regardless —
   correct per its contract, and the reason the receipt/work split matters.

## Seeded registry

`organelles.json`: 13 organelles registered (organelle-core + 12 fabric
crates), each with typed inputs/outputs, layer, mutation class, and a stable
interface digest. Registry digest: `sha256:f1115a5f…`.

## What's next

- Wire a real verifier into the driver loop (fabric-verify) so worker errors
  like #3 become VERIFY failures instead of passes.
- The outbox/inbox dead drop is ready for a real worker backend (agent
  runtime or subprocess pool).
