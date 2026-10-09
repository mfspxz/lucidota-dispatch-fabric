# RBAC research brief (2026-10-09, subagent handoff)

## 1. Formal elements (RBAC96 / NIST model)
- **Users**: human beings or active agents; each individual is known as exactly one user.
- **Roles**: bring together a collection of users *and* a collection of permissions; a role has meaning beyond any particular occupant.
- **Permissions**: positive access rights only (no denials, no duties/obligations); abstract ops (read, write, credit, debit) mapped to objects.
- **User–role assignment (UA)** and **permission–role assignment (PA)**: many-to-many relations — users never get permissions directly, only through roles.
- **Sessions**: a user opens a session and activates any *subset* of their assigned roles; permissions in a session = union of activated roles' permissions.
- **Role hierarchies (RH)**: a partial order; a senior role inherits all permissions of junior roles (inheritance flows upward), which also simplifies management — change a junior role and seniors inherit the change.
- **Constraints**: SSD (static separation of duties) forbids assigning mutually exclusive roles to the *same user*; DSD (dynamic) forbids *activating* conflicting roles within/across sessions. Both enforce the principle of least privilege and timely revocation of trust.
- **Least privilege**: a role carries only the minimum permissions needed; sessions let a user hold fewer permissions at different times (e.g., activating only the worker role, not the admin role, during a routine session).

## 2. Applying RBAC to an AI task-dispatch system

Roles: `planner` (decomposes spec → task contracts), `governor` (approves dispatch, owns policy), `worker` (executes one task contract in an isolated workspace), `verifier` (runs acceptance predicates, emits verdicts), `integrator` (merges accepted artifacts), `auditor` (read-only inspection, emits findings).

- **Whole-spec visibility**: only `planner` and `governor` sessions activate roles with `spec.read`; `worker` sessions activate a role whose only permission is `task-contract.read` on their assigned contract ID — they never see the project, matching "organelle Lego" compartmentalization.
- **Hierarchy**: `governor` senior to `integrator` senior to `worker` — seniors inherit permissions but a session may activate only junior roles (least privilege per session).
- **Static SoD**: no single agent identity is ever assigned both `worker` and `verifier` roles for the same task — this is the mechanical form of the spec's rule "prohibit the implementer's response from serving as its own review evidence."
- **Dynamic SoD**: within one session, an agent may not activate both `worker` and `integrator` roles, preventing a build from self-approving its own merge in one pass.
- **Permissions as capabilities**: `dispatch.task`, `workspace.write(scope)`, `ledger.append`, `release.approve` map 1:1 onto the dispatch fabric's external-effects gates (`push/release: policy_authorized`).

## 3. Two concrete design takeaways
1. **Session-scoped activation is the enforcement point**: permissions must be bound to the *session* (the current task attempt), not the agent identity. A worker agent gets a session token activating only `worker` on task T042 — the token, not the prompt, is what gates filesystem and ledger access.
2. **Separation of duties should be a static invariant, not a policy suggestion**: encode "worker ≠ verifier for the same task" as an SSD constraint in the scheduler so the dispatcher *cannot* construct an assignment violating it — audit-proof by construction, exactly the determinism the spec demands.

## 4. Sources
- Sandhu et al., "Role-Based Access Control Models," IEEE Computer, Feb 1996: https://www.slideserve.com/mgail/role-based-access-control-powerpoint-ppt-presentation
- RBAC96 formal model detail (UA/PA/RH/sessions): https://profsandhu.com/dissert/diss-mohammad.pdf
- NIST RBAC standard draft (SSD/DSD, least privilege, timely revocation): https://csrc.nist.gov/csrc/media/projects/role-based-access-control/documents/rbac-std-draft.pdf
- Core RBAC three rules + constrained RBAC: https://www.strongdm.com/assets/rbac-ebook.pdf
