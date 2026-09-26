#!/usr/bin/env python3
"""E0 leaf tasks (second half: T7-T11)."""

from bd_common import BUDGET_BLOCK

TASKS = [
    {
        "id": "E0-T7",
        "epic": "E0",
        "title": "Task: bootstrap-campaign — ONE-SHOT campaign pass (session-per-decision)",
        "risk": "Medium",
        "estimate": 5,
        "labels": [],
        "blocked_by": ["E0-T2", "E0-T3", "E0-T4"],
        "body": """## Objective

A single campaign session: fresh, short-lived manager invocation that reads
the reconciled board state, applies the epic-focus rule minimally (P0-first
queue ordering), selects at most one eligible task, persists exactly ONE
decision record (kind, selected task, role, model+provider, worker args,
rationale, alternatives), and exits. No-op decisions carry a typed reason.

## Thin-slice scope

THIN SLICE: one-shot pass, no daemon, no FocusState assembly beyond the
queue, no typed stop-condition taxonomy beyond no-op reasons. Full campaign
semantics deepen in E7; the always-running daemon arrives in E8.

## References

- `docs/spec/10-orchestrator.md` §9.35 (campaign orchestration: session-per-decision)
- `docs/spec/10-orchestrator.md` §9.33.3 (fresh contexts)
- `docs/spec/30-model-routing.md` §9.45 (role resolution for the manager pass)

## Acceptance criteria

- [ ] `orc campaign run --once --json` performs one pass and exits; exactly one decision record is persisted per pass
- [ ] No-op passes persist a typed reason (empty-queue | all-blocked | epic-exhausted) with the blocked graph attached for all-blocked
- [ ] The session holds no state between invocations (crash-safe by construction; next pass is fresh)
- [ ] `cargo nextest run --workspace` green; campaign tests run hermetically (sqlite board + simulator)

## QA scenarios

- Happy: seeded sqlite board with one Ready task -> pass selects it, spawns the worker via the daemon-less direct path, records the decision; evidence `.omo/evidence/<task-slug>/campaign-once.txt`
- Failure: board with only blocked tasks -> no-op with reason all-blocked + blocked graph; no worker spawned; evidence `.omo/evidence/<task-slug>/campaign-noop.txt`

## Non-goals

No polling loop (E0-T8); no budgets beyond worker-level; no epic-focus controls; no review-worker (E7).

## Security impact

Manager output is a decision record (structured, typed); worker args carry explicit capability requests only.

## Testing requirements

Hermetic integration tests; determinism test (same board -> same selection).

## Documentation impact

`docs/cli/` for `orc campaign run`; CHANGELOG `[Unreleased]`.

## Dependencies

- Blocked by E0-T2 (board read), E0-T3 (role resolution), E0-T4 (worker to spawn).

## Rollback implications

Decision records are append-only local state; a bad pass is re-runnable fresh.
""",
    },
    {
        "id": "E0-T8",
        "epic": "E0",
        "title": "Task: bootstrap-loop — simple loop runner `orc loop` (cron-shaped)",
        "risk": "Critical",
        "estimate": 5,
        "labels": ["needs-human-review"],
        "blocked_by": ["E0-T7"],
        "body": """## Objective

A simple, foreground loop runner: poll the board -> run one campaign pass ->
supervise in-flight workers -> repeat. Enforces the minimal guard set
(concurrency cap, stall timeout, attempt bounds) so the bootstrap loop cannot
run forever. This is deliberately NOT the watch daemon — no reconcile
semantics, no budget classes beyond the minimal guards, no controls surface.

## Thin-slice scope

THIN SLICE: cron-shaped foreground runner with minimal guards. The full
watch daemon (adaptive tick, stall reaping, budget classes, `orc backlog`
controls, restart recovery) deepens in E8.

## References

- `docs/spec/10-orchestrator.md` §9.36 (watch daemon — the E8 deepening)
- `docs/spec/10-orchestrator.md` §9.35 (campaign sessions the loop drives)
- `docs/spec/10-orchestrator.md` §9.24 (leases/TTLs the minimal guards approximate)

## Acceptance criteria

- [ ] `orc loop` runs pass-then-supervise cycles; SIGTERM stops cleanly within the daemon budget
- [ ] Minimal guards enforced: """ + BUDGET_BLOCK + """
- [ ] A stalled worker (no heartbeat within the stall timeout) is killed and the task transitions to a typed stalled state (never silent retry)
- [ ] `cargo nextest run --workspace` green; loop tests use a virtual clock (no real sleeps in CI)

## QA scenarios

- Happy: seeded board -> loop picks a task, runs the worker, records decision + result, next tick sees the updated board; evidence `.omo/evidence/<task-slug>/loop-happy.txt`
- Failure: worker exceeding stall timeout is reaped (process gone — assert, not just error-observed); concurrency never exceeds the cap (assert via run-state); evidence `.omo/evidence/<task-slug>/loop-guards.txt`

## Non-goals

No daemonization/systemd; no epic-focus controls; no spend budgets; no restart recovery.

## Security impact

Guard-touching (stall kills, concurrency caps, attempt bounds): Risk=Critical, needs-human-review — a weakened guard is a runaway loop.

## Testing requirements

Virtual-clock unit tests; adversarial: guardrail-weakening fixture (a config attempting to disable guards is rejected or visibly labelled).

## Documentation impact

`docs/cli/` for `orc loop`; CHANGELOG `[Unreleased]`.

## Dependencies

- Blocked by E0-T7 (campaign pass).

## Rollback implications

Foreground process; stopping it leaves board state intact (board-wins on next run).
""",
    },
    {
        "id": "E0-T9",
        "epic": "E0",
        "title": "Task: bootstrap-reporting — failure-driven Bug auto-file at attempts-exhaustion + blocking-fix exception (IS-14)",
        "risk": "Low",
        "estimate": 3,
        "labels": [],
        "blocked_by": ["E0-T7"],
        "body": """## Objective

When a task exhausts its attempt budget on a reproducible failure, the loop
files a Bug at Status=Triage with structured failure context (normalized
failure class per §9.33.5, evidence pointers, task/session links) alongside
the task's blocked/needs-human transition. Implement the report-≠-self-fix
rule with its single exception: a defect BLOCKING the reporting agent's
current task is fixed in that task's own PR with a regression test; if too
large, the Bug becomes a blockedBy dependency.

## Thin-slice scope

THIN SLICE: failure-driven auto-file (a) + blocking-fix exception only. The
discretionary `report_issue` tool (b) lands with the coordinator tools in
E3; ownership-routed arbitraitor gaps keep the `blocked:arbitraitor` flow
(E5-T6 owns the helper).

## References

- `docs/spec/10-orchestrator.md` §9.37 (agent issue reporting — (a) failure-driven auto-file; report ≠ self-fix; blocking-fix exception)
- `docs/spec/10-orchestrator.md` §9.33.5 (failures and retries — failure classes)
- `docs/spec/10-orchestrator.md` §9.40 (blockedBy dependency for too-large fixes)

## Acceptance criteria

- [ ] A task exhausting its attempt budget on a reproducible failure produces: (1) a Bug filed at Status=Triage with class, evidence pointers, task/session links, and (2) the task's transition to blocked/needs-human — both visible on the board
- [ ] The reporting agent does NOT pick up its own reported bug (file-and-continue/end-task default)
- [ ] Blocking-fix exception: a blocking defect fixed in-task lands with a regression test and closes/links the Bug; a too-large fix becomes a blockedBy edge instead
- [ ] Agent-authored issue bodies are marker-wrapped untrusted content (§6.1/§9.37(d)) — never executed, never auto-scheduled
- [ ] `cargo nextest run --workspace` green including auto-file unit tests

## QA scenarios

- Happy: fixture task failing reproducibly past its budget -> Bug exists at Triage with structured context; task blocked; evidence `.omo/evidence/<task-slug>/autofile-happy.txt`
- Failure: injection attempt in the failure context (instructions addressed to the PM embedded in the error text) appears in the Bug body as quoted data only — no field beyond title/class/description is influenced; evidence `.omo/evidence/<task-slug>/autofile-injection.txt`

## Non-goals

No discretionary reporting tool (E3); no auto-triage/promotion of filed Bugs (triage is a human/PM gate).

## Security impact

Untrusted content boundary: agent-authored bodies are data. Marker-wrapping + injection hardening required.

## Testing requirements

Negative/adversarial tests for the injection boundary; unit tests for exhaustion detection.

## Documentation impact

Workflow docs note (Bug auto-file semantics); CHANGELOG `[Unreleased]`.

## Dependencies

- Blocked by E0-T7 (campaign pass detects exhaustion).

## Rollback implications

Filed Bugs are closable; the transition is a board field write (revertible on the board).
""",
    },
    {
        "id": "E0-T10",
        "epic": "E0",
        "title": "Task: bootstrap-mcp-search — worker search/code-intelligence via approved MCP servers (D10 MCP-early)",
        "risk": "Medium",
        "estimate": 5,
        "labels": [],
        "blocked_by": ["E0-T4"],
        "body": """## Objective

Satisfy the worker's search/code-intelligence needs from day one via
approved, fingerprinted MCP servers instead of native indexing: a
codegraph-style symbol/call-graph server and a codebase-memory-style
knowledge-graph server, wired into the worker's search tool. Servers are
untrusted principals under §9.18.1 containment: fingerprinted per session,
launched through Arbitraitor inspection, contained in a Restricted sandbox
minimum; MCP annotations are advisory input only.

## Thin-slice scope

THIN SLICE: two approved servers behind the worker search tool. The built-in
MCP PROXY (one endpoint fronting built-ins + externals + arbitraitor's own
server) deepens in E4; native indexing paths mature later per §9.15/§9.16.

## References

- `docs/spec/10-orchestrator.md` §9.38 (MCP-early tool strategy)
- `docs/spec/20-harness-worker.md` §9.18.1 (MCP and tool drift — fingerprinting, advisory annotations)
- `docs/spec/40-arbitraitor-integration.md` §9.6 (sandbox containment for launched servers)
- Ledger D10 (MCP-early + proxy decision; pulls knowledge-index federation forward from §999/M4)

## Acceptance criteria

- [ ] Worker search tool resolves queries through the two approved MCP servers; results are marked with their source server
- [ ] Each server is fingerprinted (executable digest, per-tool schema digests) before first use; a digest change re-triggers approval (schema-drift detection)
- [ ] Servers launch inside Arbitraitor-reported containment (Restricted minimum); annotations never grant authority
- [ ] Tool results quoted into agent context pass the `sanitize_for_agent` boundary
- [ ] `cargo nextest run --workspace` green; server-fingerprint unit tests with fixture servers

## QA scenarios

- Happy: search query returns symbol/call-graph results tagged with the serving server + fingerprint; evidence `.omo/evidence/<task-slug>/mcp-search-happy.txt`
- Failure: a server whose schema digest changed since approval is quarantined (calls refused, drift event recorded) — the server's own `readOnly` annotation does not restore it; evidence `.omo/evidence/<task-slug>/mcp-drift-quarantine.txt`

## Non-goals

No MCP proxy component (E4); no new search indexing in Orchestraitor; no trust in server claims.

## Security impact

Untrusted-server boundary: fingerprinting + containment + advisory-annotations-only. Authority always from Arbitraitor's analyzer.

## Testing requirements

Fingerprint/drift unit tests; negative test: annotation-claimed `readOnly` server attempting a write is blocked by policy, not by its claim.

## Documentation impact

Approved-server configuration docs; CHANGELOG `[Unreleased]`.

## Dependencies

- Blocked by E0-T4 (worker toolset).

## Rollback implications

Servers are config-approved (removable); search falls back to no-results with a typed reason, never to an unmediated path.
""",
    },
    {
        "id": "E0-T11",
        "epic": "E0",
        "title": "Task: bootstrap-review — wire draft-PR review trigger to the EXISTING adversarial-review policy (no autonomous reviewer in E0)",
        "risk": "Low",
        "estimate": 2,
        "labels": [],
        "blocked_by": ["E0-T6"],
        "body": """## Objective

Connect the loop's draft PRs to the repo's existing fresh-context
adversarial-review workflow (AGENTS.md review and merge invariants;
`.agents/skills/github-pr-lifecycle`): the loop moves the board item to
In Review, requests review per the workflow policy, and tracks convergence —
it does NOT implement a reviewer. The autonomous review-worker (IS-6 leg)
lands under E7; until then review is the existing policy ridden as-is.

## Thin-slice scope

THIN SLICE: trigger + board tracking only. The autonomous review-worker
deepens in E7 (E7-T5).

## References

- AGENTS.md (review and merge invariants — the authority this task rides)
- `.agents/project/orchestraitor-workflow.md` (PR convergence requirements)
- `docs/spec/10-orchestrator.md` §9.33.4 (change-set and PR review loop)
- `docs/spec/10-orchestrator.md` §9.33.3 (fresh-context reviewers)

## Acceptance criteria

- [ ] When a worker opens a draft PR, the board item transitions to In Review and a review is requested per the workflow policy (reviewer selection stays manual/policy-driven in E0)
- [ ] The loop never merges, never approves, and never dismisses reviews — merge stays human-gated
- [ ] Convergence state is tracked from board/PR facts (review threads resolved, checks green) without the loop acting on them
- [ ] `cargo nextest run --workspace` green including trigger unit tests

## QA scenarios

- Happy: fixture draft PR -> item In Review + review requested; evidence `.omo/evidence/<task-slug>/review-trigger.txt`
- Failure: a PR with an unresolved blocking thread is never treated as converged (state stays In Review); evidence `.omo/evidence/<task-slug>/no-false-convergence.txt`

## Non-goals

No reviewer agent; no review-loop automation beyond trigger + tracking; no merge authority.

## Security impact

None new (rides existing policy); the loop's non-merge invariant is asserted in E10-T4.

## Testing requirements

Unit tests for the trigger; negative test for false convergence.

## Documentation impact

Workflow docs note (loop rides existing review policy until E7); CHANGELOG `[Unreleased]`.

## Dependencies

- Blocked by E0-T6 (draft PR exists first).

## Rollback implications

Trigger is a board transition + review request — both revertible.
""",
    },
]
