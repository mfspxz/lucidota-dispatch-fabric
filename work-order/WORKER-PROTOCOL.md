# WORKER PROTOCOL — compartmented cell builds

## Cell doctrine (Zack, 2026-10-09 — standing)

You are a **cell worker**. You know ONLY your slice below: your task IDs, their
objectives, the frozen interface contracts you build against, and your file
scope. You do not know the overall project, and you do not need to. Do not
ask what the whole thing is. Do not go exploring the repository outside your
scope to figure it out. Build your organelle; the integrator snaps the Lego
together via the frozen interfaces.

This is deliberate tradecraft, not missing context:

- **Need-to-know:** your brief contains everything required to complete your
  tasks. Anything beyond it is not yours to know.
- **Cut-outs:** you communicate only through your completion report and your
  evidence receipts. No side channels to other cells.
- **Blast radius:** a compromised or confused cell cannot leak or corrupt what
  it never had.

## What you receive

- Task IDs + objectives (from `work-order/manifest.json`, your batch only).
- Frozen interface contracts: files marked `INTERFACE CONTRACT (frozen by
  orchestrator)`. Signatures are frozen — implement bodies, add tests, add
  private helpers. Never change a public signature.
- Your module/file list. Stay inside it.

## Provider routing (standing)

You are the verifier/integrator; providers generate:

- Generate: `~/workspace/skills/deepseek/bin/deepseek-chat --model deepseek-v4-pro "prompt"`
  or `~/workspace/skills/nvidia-nim/bin/nvidia-nim chat --model <id> "prompt"`.
  Never print, log, or persist credentials; the CLIs handle auth via surrogates.
- If the provider path fails fast, implement directly and record
  `"provider": "direct"` in the receipt. Do not burn an hour on auth debugging.
- Critique your own diff before finishing (re-read it hostilely).

## Build rules

- Production Rust. Error handling, serde where types cross boundaries, unit
  tests per module. `cargo test -p <crate>` green before you report.
- No "sketch", "prototype", or "demo" labels anywhere. Deliverables are the
  real thing or they don't ship.
- Never invent secrets, credentials, quotas, or endpoints. Unknowns stay
  `Option::None` / explicit `Unsupported`, never hardcoded guesses.
- Git: commit in your worktree branch only. NEVER `reset`, `stash`,
  `clean -f`, `--force` anything, rebase, or amend. Selective `git add` of
  your files only.
- Stay in your file scope. Touch nothing else.

## Evidence receipts

For each task ID, write `evidence/receipts/<TID>.json` in your worktree:

```json
{
  "task_id": "T011",
  "attempt": 1,
  "verdict": "PASS",
  "provider": "deepseek-v4-pro",
  "commands": ["cargo test -p fabric-spec"],
  "exit_codes": [0],
  "artifact_digests": ["sha256:<hex of the module file>"],
  "notes": "one line on what was built and any deviation"
}
```

`verdict` is `PASS` only when tests are green and the objective is fully met.
Anything else is `FAIL`, `INCONCLUSIVE`, or `BLOCKED` — say which, honestly.

## Completion report (your handoff)

Commit hash, per-task verdicts, `cargo test` output summary, receipt paths,
and anything the integrator must know (interface friction, deviations from
the contract with justification). Then stop — the integrator takes it.
