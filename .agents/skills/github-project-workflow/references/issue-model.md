# Issue model

The issue model is the contract between the github-project-workflow skill and the project's GitHub configuration. Field names and option values below are the **defaults** committed in `github-project.example.toml`; a real project may rename them — the scripts read the local config, never hardcode these.

## Type (org-level native issue type)

- `Epic`, `Feature` — decomposable. NOT directly implementable. Must be split into leaf sub-issues before any work starts.
- `Task`, `Bug` — leaf, directly implementable (once Ready).
- `Spike` — time-boxed investigation producing a decision, not shipped code. If it concludes with implementation work, open Tasks separately.

GitHub native issue types are configured at the **organization level** (GA Apr 2025). They inherit down to repos. There is no universal default list: the org defines its own. The project config declares which org-configured type names mean what here.

## Target

- `MVP` — references a requirement in `docs/spec/spec.md` §998 (MVP requirements) or a §9 subsystem the MVP depends on.
- `Next` — references spec §999 high-value differentiators. **Never scheduled during the MVP phase.**
- `Later` — references spec §999 lower-priority post-MVP work. Not scheduled during MVP.
- `Icebox` — unscheduled. Not schedulable.

During MVP delivery, only `Target = MVP` work may enter the ready-queue (workflow policy).

## Status (project single-select field)

Match the live "Arbsec Development" project (verified 2026-07-23):

```text
Triage       → newly opened; not yet classified
Backlog      → Type/Target/acceptance criteria not yet finalized
Ready        → Definition of Ready met (see definitions-of-ready-and-done.md) — the ONLY schedulable state
In Progress  → claimed by an agent (assignee = @me or another agent)
In Review    → PR open, adversarial review loop running
Done         → merged, closed, post-merge reconciliation complete
```

**No "Blocked" Status** exists on the live project. Blocking is tracked via the native
`blockedBy` edge (GA Aug 2025) — the ready-queue script filters on it directly. A blocked
issue stays in its current Status (e.g. `In Progress`) and is excluded from the ready-queue
by its unresolved `blockedBy` edge.

## Risk (single-select, matches live project)

`Low`, `Medium`, `High`, `Critical`.

`Critical` forces human review before release (spec §21.1) and routes to
`@arbsec/security` via CODEOWNERS.

## Domain and Autonomy

The live project does not currently have Domain or Autonomy fields. These are planned for
future addition; until then, reviewer-domain selection (spec §9.33.4) is driven by labels and
changed-file paths, and autonomy defaults to `manual` for anything touching the Arbitraitor
integration boundary or security-sensitive paths.

## Leaf vs. decomposable (the rule)

```text
Epic, Feature   → MUST decompose into leaf sub-issues first
Task, Bug       → directly implementable (if Ready)
Spike           → investigation only; output is a finding/ADR, not mergeable code
```

Decomposition uses **native GitHub sub-issues** (the `parent` / `subIssues` relationship, GA Apr 2025). See `task-decomposition.md`.

## Blocked-by (native issue dependency, GA Aug 2025)

A `blockedBy` edge means "this issue cannot start until that issue lands". It is NOT a transient/rate-limit/blocker; it is a hard DAG edge. The ready-queue excludes any issue with an unresolved `blockedBy` (workflow policy).

Cross-repo blockers (Arbitraitor): open the canonical issue in `arbsec/arbitraitor`, then link from the Orchestraitor issue. The Orchestraitor issue carries a `blockedBy` edge (or body link for cross-repo) and is excluded from the ready-queue until the upstream PR lands (spec §16.2). Do NOT retry a `blocked:arbitraitor` issue as if it were transiently blocked (spec §9.26.1).

## Where this lives in `gh`

`gh issue list --json` returns these fields (gh v2.94.0+, verified 2026-07-23): `issueType`, `parent`, `subIssues`, `subIssuesSummary`, `blockedBy`, `blocking`, `labels`, `assignees`, `projectItems`.

Note: `blockedBy`, `blocking`, `subIssues` are returned as `{ nodes: [...], totalCount: N }` (not flat arrays) so consumers can detect pagination. See `gh-capabilities.md`.
