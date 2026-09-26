#!/usr/bin/env python3
"""E1 leaf tasks: board abstraction + providers + multi-org workspaces."""

TASKS = [
    {
        "id": "E1-T1",
        "epic": "E1",
        "title": "Task: board-contract — `BoardProvider` trait + local read-cache semantics",
        "risk": "Low",
        "estimate": 5,
        "labels": [],
        "blocked_by": [],
        "body": """## Objective

Define the `BoardProvider` contract per §9.43: items (stable identity, type,
title, body), statuses, typed custom fields, dependency edges (native
blockedBy, cross-board within a workspace), cross-references, and search.
Implement the single-canonical-provider semantics: work items live ON the
provider; Orchestraitor keeps a local read-cache that is never authoritative
(write-through then refresh); runtime state stays local-only keyed by stable
board item identity.

## References

- `docs/spec/10-orchestrator.md` §9.43 (kanban board abstraction — full contract)
- `docs/spec/10-orchestrator.md` §9.40 (blockedBy edges are the DAG — no mirrored second graph)
- Ledger D5 (state/sync semantics: board-wins, `board-diverged` events, last-synced stamps)

## Acceptance criteria

- [ ] The trait covers all six contract areas (items/statuses/fields/edges/cross-refs/search) with typed errors; `cargo doc` renders without warnings
- [ ] A reference in-memory implementation serves the test suite; cache semantics tested: write-through then refresh, stale reads stamped with last-synced
- [ ] `cargo nextest run --workspace` green including contract conformance tests

## QA scenarios

- Happy: conformance suite passes against the in-memory provider (items, field writes, edges, search); evidence `.omo/evidence/<task-slug>/contract-conformance.txt`
- Failure: a provider returning a divergent state after a local write triggers a board-wins reconcile + `board-diverged` event (assert the event, not just the absence of a crash); evidence `.omo/evidence/<task-slug>/board-diverged.txt`

## Non-goals

No GitHub/sqlite implementations (follow-up tasks); no UI.

## Security impact

Board content is untrusted input (§6.1); the contract treats bodies as opaque data.

## Testing requirements

Conformance test suite reusable by all providers; negative tests for cache-authority violations.

## Documentation impact

Contract docs (trait-level); CHANGELOG `[Unreleased]`.

## Dependencies

None.

## Rollback implications

New trait — additive; no existing behavior changed.
""",
    },
    {
        "id": "E1-T2",
        "epic": "E1",
        "title": "Task: board-sqlite — local sqlite board provider (CI/offline)",
        "risk": "Low",
        "estimate": 5,
        "labels": [],
        "blocked_by": ["E1-T1"],
        "body": """## Objective

Implement the `BoardProvider` contract over a local sqlite database: the
CI/offline path so tests never hit the network (deterministic-simulator
rule, §21.3). Supports the full contract surface including dependency edges
and cross-references within the local board.

## References

- `docs/spec/10-orchestrator.md` §9.43 (providers — local sqlite for CI and offline)
- `docs/spec/50-contracts-data.md` §21.3 (deterministic simulator; CI never depends on live services)
- Ledger D5 (single canonical provider per deployment — sqlite is an alternative canonical, not a mirror)

## Acceptance criteria

- [ ] The E1-T1 conformance suite passes against the sqlite provider
- [ ] Board fixtures can be seeded/inspected via a dev command (`orc board fixture load/list` or test helper)
- [ ] `cargo nextest run --workspace` green with zero network access in the test process (assert via test harness sandbox)

## QA scenarios

- Happy: seeded sqlite board drives a full hermetic campaign pass (with E0/E7 components) offline; evidence `.omo/evidence/<task-slug>/sqlite-hermetic.txt`
- Failure: corrupted sqlite file -> typed storage error with recovery guidance, no panic; evidence `.omo/evidence/<task-slug>/sqlite-corrupt.txt`

## Non-goals

No sync to GitHub; no multi-board semantics (single board).

## Security impact

Local-only store; no credentials.

## Testing requirements

Conformance suite + migration test for schema changes.

## Documentation impact

Provider docs; CHANGELOG `[Unreleased]`.

## Dependencies

- Blocked by E1-T1 (contract).

## Rollback implications

Database file is deletable; no external state.
""",
    },
    {
        "id": "E1-T3",
        "epic": "E1",
        "title": "Task: board-github — GitHub Projects v2 provider (full surface, cached node IDs, adaptive rate-limit tick)",
        "risk": "Medium",
        "estimate": 8,
        "labels": [],
        "blocked_by": ["E1-T1"],
        "body": """## Objective

Full-depth GitHub Projects v2 provider: all contract areas over Projects v2
GraphQL (items, fields, dependency edges, cross-references, search), with
org/project/field/option node IDs resolved at runtime and cached OUTSIDE the
repository, and a poll tick that adapts to rate-limit feedback (secondary
limits honored; backoff on 403/429 class responses).

## References

- `docs/spec/10-orchestrator.md` §9.43 (GitHub Projects v2 first provider; cached node IDs; adaptive tick)
- `.agents/project/github-project.example.toml` (field/option names; node-ID caching convention)
- Ledger F27 (GraphQL shapes; `projects_v2_item` webhook actions — optional later trigger)

## Acceptance criteria

- [ ] The E1-T1 conformance suite passes against the GitHub provider (live run, evidence captured; CI uses recorded fixtures)
- [ ] Node-ID cache lives outside the repo (XDG cache dir); no node IDs in any tracked file (grep-verified in CI)
- [ ] Rate-limit feedback adapts the tick cadence; secondary-rate-limit responses back off, never hammer
- [ ] `cargo nextest run --workspace` green (fixture-backed); live smoke evidence recorded

## QA scenarios

- Happy: full conformance pass against the shared board (read-only assertions + one scratch status write on a test item, reverted); evidence `.omo/evidence/<task-slug>/github-conformance.txt`
- Failure: simulated 403 secondary-rate-limit fixture -> cadence backs off (assert inter-request delay grew); malformed item body -> skipped with warning; evidence `.omo/evidence/<task-slug>/github-ratelimit.txt`

## Non-goals

No webhooks (polling-first per D5); no writes beyond contract surface; no board schema changes.

## Security impact

Token handling via the service identity (E0-T1); board content untrusted.

## Testing requirements

Fixture unit tests + live smoke; rate-limit adaptation tests with a virtual clock.

## Documentation impact

Provider docs incl. cache location; CHANGELOG `[Unreleased]`.

## Dependencies

- Blocked by E1-T1 (contract).

## Rollback implications

Read-mostly; writes are board transitions revertible on the board.
""",
    },
    {
        "id": "E1-T4",
        "epic": "E1",
        "title": "Task: board-queue — ready-queue view: epic-focus predicate + assignee exclusion + eligibility filters",
        "risk": "Medium",
        "estimate": 5,
        "labels": [],
        "blocked_by": ["E1-T1"],
        "body": """## Objective

The ready-queue view over any `BoardProvider`: promote only leaf Tasks/Bugs of
the active epic (priority-ordered P0-P3) that satisfy the workflow-policy
eligibility rules (Target=MVP, Status=Ready, no unresolved blockedBy, no
conflicting in-flight PR), with bug-preemption ordering (standalone Bugs
always preempt unless epic-linked) and human-exclusion (items assigned to
humans are excluded; the service-identity set is declared in configuration,
never inferred).

## References

- `docs/spec/10-orchestrator.md` §9.41 (epic-focus scheduling policy — queue predicate, bug preemption, human exclusion)
- `.agents/project/orchestraitor-workflow.md` (MVP-only scheduling rules)
- Ledger D11 (service-identity set: the App bot slug + configured machine identities)

## Acceptance criteria

- [ ] `orc board ready --json` returns exactly the eligible items for a seeded board (epic-focus + priority order + bug preemption + exclusions)
- [ ] Assignee exclusion: items assigned to any human are excluded; items assigned to a configured service identity are schedulable; unassigned items are schedulable
- [ ] blockedBy filtering uses the provider's native edges (no shadow DAG)
- [ ] `cargo nextest run --workspace` green including queue-predicate unit tests over sqlite fixtures

## QA scenarios

- Happy: seeded board (active epic + standalone Bug + human-assigned item + blocked item) -> queue lists Bug first, then epic tasks by priority, excluding the human-assigned and blocked items; evidence `.omo/evidence/<task-slug>/queue-predicate.txt`
- Failure: dependency cycle among seeded items -> cycle named in a needs-human report, no item from the cycle scheduled; evidence `.omo/evidence/<task-slug>/queue-cycle.txt`

## Non-goals

No focus controls (E8); no stall views (E1-T5); no scheduling (selection is the campaign's job).

## Security impact

Queue correctness is a scheduling-safety property (wrong queue = wrong work executed); no security primitive.

## Testing requirements

Property-style tests over fixture boards; cycle-detection negative test.

## Documentation impact

Queue semantics docs; CHANGELOG `[Unreleased]`.

## Dependencies

- Blocked by E1-T1 (contract); consumes E0-T1's service-identity config.

## Rollback implications

View-only; no state mutated.
""",
    },
    {
        "id": "E1-T5",
        "epic": "E1",
        "title": "Task: board-reconcile — stall + reconcile views (board-wins, stale-blocker drift, cycle detection)",
        "risk": "Medium",
        "estimate": 5,
        "labels": [],
        "blocked_by": ["E1-T1"],
        "body": """## Objective

Reconcile and stall views over the board abstraction: every poll tick is a
reconcile with board-wins semantics (`board-diverged` events; last-synced
stamps), stale-blocker drift surfacing (blocker Done but edge unresolved ->
visible report, never silently rewritten), and dependency-cycle detection
producing a needs-human report naming the cycle's members.

## References

- `docs/spec/10-orchestrator.md` §9.43 (reconcile semantics)
- `docs/spec/10-orchestrator.md` §9.40 (cycles; stale-blocker drift)
- `docs/spec/10-orchestrator.md` §9.36 (poll tick = reconcile pass)
- Ledger D4 (blocked/dependency semantics: cycles, drift, hard waits)

## Acceptance criteria

- [ ] A reconcile pass over a diverged state produces a `board-diverged` event and adopts the board's value
- [ ] Stale-blocker drift (blocker Done, edge unresolved) is surfaced as a report; the edge is NOT rewritten or dropped
- [ ] Cycle detection names all members and produces a needs-human report; cyclic tasks are never scheduled
- [ ] `cargo nextest run --workspace` green including reconcile unit tests

## QA scenarios

- Happy: divergent local/board fixture -> board wins + event recorded; evidence `.omo/evidence/<task-slug>/reconcile-wins.txt`
- Failure: cyclic fixture -> needs-human report with member list; no auto-break; drift fixture -> report only; evidence `.omo/evidence/<task-slug>/cycle-and-drift.txt`

## Non-goals

No automatic edge repair; no stall reaping (E8 owns the reaper; this task provides the views).

## Security impact

None (view layer); correctness-critical for scheduling safety.

## Testing requirements

Unit tests with fixture graphs; negative tests for silent rewrites (assert edge unchanged).

## Documentation impact

Reconcile semantics docs; CHANGELOG `[Unreleased]`.

## Dependencies

- Blocked by E1-T1 (contract).

## Rollback implications

View-only; events are append-only local state.
""",
    },
    {
        "id": "E1-T6",
        "epic": "E1",
        "title": "Task: board-workspace — multi-org workspaces (N boards, per-board credentials)",
        "risk": "Medium",
        "estimate": 8,
        "labels": [],
        "blocked_by": ["E1-T1"],
        "body": """## Objective

Workspace configuration owning multiple organizations and their projects —
each project a board-provider instance with its own credentials (never a
shared ambient token). One daemon run serves exactly one active workspace;
switching is an explicit daemon-level action.

## References

- `docs/spec/10-orchestrator.md` §9.42 (multi-org workspaces and cross-project epics)
- `docs/spec/40-arbitraitor-integration.md` §9.23 (secret resolution per credential)
- Ledger D5 (single canonical provider per workspace)

## Acceptance criteria

- [ ] A workspace config with two boards (e.g. one sqlite + one GitHub fixture) instantiates two provider instances with distinct credential resolution
- [ ] No ambient token use: each provider resolves its own credentials via `secret://` URIs
- [ ] One active workspace per run; switching is explicit and logged
- [ ] `cargo nextest run --workspace` green including workspace config unit tests

## QA scenarios

- Happy: two-board workspace -> both boards readable, items keyed by (provider, stable id); evidence `.omo/evidence/<task-slug>/workspace-two-boards.txt`
- Failure: a board with unresolvable credentials fails closed for that board only (other board unaffected, error typed); evidence `.omo/evidence/<task-slug>/workspace-cred-fail.txt`

## Non-goals

No cross-board epics (E1-T7); no credential UI.

## Security impact

Per-board credential isolation — a compromised board credential must not reach another board.

## Testing requirements

Negative test: credential isolation (board A's token cannot address board B).

## Documentation impact

Workspace configuration docs; CHANGELOG `[Unreleased]`.

## Dependencies

- Blocked by E1-T1 (contract).

## Rollback implications

Config-layer change; removable via `orc config unset`.
""",
    },
    {
        "id": "E1-T7",
        "epic": "E1",
        "title": "Task: board-cross-epic — cross-project epics (anchor board + cross-board membership edges)",
        "risk": "Medium",
        "estimate": 5,
        "labels": [],
        "blocked_by": ["E1-T6"],
        "body": """## Objective

Connectable projects: an Epic lives on an anchor board and may span multiple
projects; its Tasks/Sub-tasks are targeted at specific projects through
cross-board membership edges. Epic-focus, blockedBy edges, and bug-preemption
resolve ACROSS all boards of the workspace (the epic's sub-graph is the union
of its member items, not a copy).

## References

- `docs/spec/10-orchestrator.md` §9.42 (cross-project epics; workspace-wide resolution)
- `docs/spec/10-orchestrator.md` §9.40 (blockedBy may cross boards within the workspace)
- `docs/spec/10-orchestrator.md` §9.41 (focus/preemption resolution across boards)

## Acceptance criteria

- [ ] An epic on the anchor board with member tasks on a second board resolves its sub-graph as the union (no copies)
- [ ] A blockedBy edge from a task on board A to an issue on board B gates the task within the workspace
- [ ] Epic-focus treats the cross-board sub-graph as one queue; bug-preemption applies to standalone Bugs on any board
- [ ] `cargo nextest run --workspace` green including cross-board fixture tests

## QA scenarios

- Happy: two-board fixture with a cross-board epic -> focus queue includes member tasks from both boards in priority order; evidence `.omo/evidence/<task-slug>/cross-board-focus.txt`
- Failure: a cross-board blocker that closes on board B promotes the board-A task on the next reconcile (assert promotion); a cross-board cycle is reported, not scheduled; evidence `.omo/evidence/<task-slug>/cross-board-blocks.txt`

## Non-goals

No cross-workspace resolution; no board-data copying.

## Security impact

None new (resolution layer over existing providers).

## Testing requirements

Cross-board fixture tests; promotion + cycle negatives.

## Documentation impact

Cross-project epic docs; CHANGELOG `[Unreleased]`.

## Dependencies

- Blocked by E1-T6 (workspaces).

## Rollback implications

Edges are board data (revertible on the boards); no migration.
""",
    },
    {
        "id": "E1-T8",
        "epic": "E1",
        "title": "Task: board-migration — `orc board import/export` provider switching",
        "risk": "Low",
        "estimate": 3,
        "labels": [],
        "blocked_by": ["E1-T2", "E1-T3"],
        "body": """## Objective

Explicit provider-switching migration: `orc board import/export` moves board
content between providers (e.g. sqlite -> GitHub Projects v2) as a one-shot
migration with a dry-run diff — never a live mirror (single canonical
provider per deployment).

## References

- `docs/spec/10-orchestrator.md` §9.43 (switching is an explicit migration, not a mirror)
- Ledger D5 (provider switching semantics)

## Acceptance criteria

- [ ] `orc board export --provider sqlite --out <file>` writes a complete, versioned snapshot (items, fields, edges)
- [ ] `orc board import --provider github --in <file> --dry-run` shows the exact writes; without `--dry-run` applies them idempotently (re-run is a no-op)
- [ ] `cargo nextest run --workspace` green including round-trip unit tests

## QA scenarios

- Happy: sqlite -> file -> sqlite round-trip preserves items/fields/edges (assert equality); evidence `.omo/evidence/<task-slug>/roundtrip.txt`
- Failure: import with a conflicting item (same stable id, different content) -> typed conflict error listing conflicts, nothing written; evidence `.omo/evidence/<task-slug>/import-conflict.txt`

## Non-goals

No continuous sync; no GitHub-to-GitHub project moves.

## Security impact

Export files contain board content (untrusted data) — treated as data on import; no credentials in exports.

## Testing requirements

Round-trip + conflict tests.

## Documentation impact

`docs/cli/` for import/export; CHANGELOG `[Unreleased]`.

## Dependencies

- Blocked by E1-T2, E1-T3 (both providers exist).

## Rollback implications

Dry-run first; imports are idempotent and auditable.
""",
    },
]
