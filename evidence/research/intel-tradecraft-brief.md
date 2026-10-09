# Intelligence tradecraft compartmentalization — research brief (2026-10-09, subagent handoff)

## Key mechanisms (tradecraft → worker mapping)

1. **Need-to-know / compartmentalization** (CIA, KGB): officers know only operations they participate in; a failure in one compartment doesn't spill into others. → Worker prompts carry only the leaf task contract — objective, inputs, outputs, acceptance predicates — never the master spec, sibling tasks, or project name.

2. **Codeword classification** (e.g. "Top Secret Ultra"): even the highest-cleared are denied codeword material without a specific read-in. → Sensitivity tiers per task: a worker is "read in" only to its task's tier, and tier assignment itself is never explained to the worker.

3. **Cut-outs**: the intermediary knows only source and destination of the material, not the identities of anyone else in the chain; a captured cut-out can't burn the network. → The dispatcher is the sole holder of the full task graph; workers receive a dispatch envelope and return artifacts, never learning upstream/downstream task IDs or who consumed their output.

4. **Clandestine cell structure**: each small cell knows only its own members; one captured/interrogated cell can't compromise the organization — harm is bounded by design. → One isolated workspace (git worktree) per task: a leaking or compromised worker's blast radius is its own tree; no shared checkouts, no cross-task file reads.

5. **Covert vs. clandestine (NATO definitions)**: covert = sponsor concealed; clandestine = the *operation itself* concealed from the participants. → Go past need-to-know: write prompts so the worker can't infer the project even from its own task (generic phrasing, renamed entities) — the operation is hidden from the participant, not just the sponsor.

6. **Dead drops / couriers**: agents never meet; material is left at and collected from neutral handoff points. → The artifact store is the dead drop: workers write content-addressed artifacts; the integrator collects by digest. No worker-to-worker channels ever exist.

## Design takeaways for the dispatcher/governor

1. **The governor holds the only complete view** (spec, DAG, integration plan) and must actively redact when rendering prompts: render from the task contract + shared contracts only, never from master-spec text; verifier error messages must not leak sibling-task information back to the worker.
2. **Enforce one-way information flow**: all cross-task linking (traceability, integration, coverage) happens inside the governor. If a task needs a prior artifact, the governor embeds only the artifact bytes — stripped of provenance metadata (which worker/task produced it, what project it served).

## Sources

- https://en.wikipedia.org/wiki/Compartmentalization_(intelligence) — need-to-know, codeword classification, Manhattan Project example
- https://wikispooks.com/wiki/Cut-out — cut-out definition: knows only source and destination
- https://en.wikipedia.org/wiki/Covert_cell — clandestine cell system; capture of one cell can't compromise the whole
- https://graphsearch.epfl.ch/en/concept/14558092 — clandestine cell system; NATO covert (sponsor concealed) vs. clandestine (operation concealed from participants)
