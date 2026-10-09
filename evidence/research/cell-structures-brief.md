# Clandestine cell structures — organizational-design brief (2026-10-09, subagent handoff)

## Key mechanisms
1. **Need-to-know compartmentalization** — cell members know only their own cell's identities and task; higher echelons and sibling cells are invisible by design (Wikipedia, clandestine cell system).
2. **Cut-outs / brokers** — intermediaries relay messages and resources between cells without revealing endpoints; the broker is the only bridge node, deliberately creating information bottlenecks (OSS Secret Intelligence Manual; mafia-network studies on brokers).
3. **Codenames and indirect coordination** — planning and coordination proceed via aliases and relayed directives so compromising one node yields no real identities or locations (Baccara, "How to Organize Crime").
4. **Small cells with independent reporting** — networks are kept small and subdivided, each reporting upward independently rather than laterally, so one compromised unit can't enumerate the whole network (Suvorov on Soviet illegals).
5. **Functional compartmentalization via impersonal communications** — dead drops, relayed messages, and one-way channels replace person-to-person contact, minimizing the "signature" of coordination (JSOU Report 12-3, cyberwar.nl).
6. **Burn containment** — if a cell is infiltrated, damage is confined to that cell; adjacent cells detect the cut-out going dark and sever contact, no domino effect (vectree networked-criminal-architecture summary).

## Mapping to AI task-worker isolation
1. Worker prompts contain only their own task contract — never project name, sibling tasks, or end goal.
2. A dispatcher/court layer mediates all context; workers never address each other directly — no lateral channels, only vertical handler-to-worker.
3. Task briefs use opaque IDs (T001, artifact digests) instead of descriptive project vocabulary, so a leaked prompt can't be reassembled into the whole.
4. Workers are stateless, single-task, and ephemeral; no persistent worker identity accumulates project knowledge across tasks.
5. Worker input/output schemas are the dead drops — structured JSON in/out, no free-form chat between workers and orchestrator.
6. On bad output, quarantine the worker's artifacts and rotate its credential/cell slot; never let one failed worker's context bleed into the next dispatch.

## 2 concrete design takeaways
1. **RBAC as the cell membrane:** each worker's credential grants access to exactly one task scope (one worktree, one receipt ledger append, one artifact namespace) — the dispatch system enforces the compartment; the worker never self-describes or self-scopes.
2. **The orchestrator is the only entity with the full map:** keep the DAG, traceability graph, and requirement coverage in a governor/court layer; workers receive rendered one-shot prompts with a digest, never the manifest — assemble the whole only at integration, from signed receipts.

## Sources
- https://en.wikipedia.org/wiki/Clandestine_cell_system
- https://cyberwar.nl/d/norobots/tradecraft-howto.pdf
- http://en.wikipedia.org/wiki/Clandestine_HUMINT_operational_techniques
- https://www.mariagiovannabaccara.com/crimeRES.pdf
- https://vectree.io/pdf/c/networked-criminal-architecture
