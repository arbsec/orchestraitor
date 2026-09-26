#!/usr/bin/env python3
"""Shared constants + epic definitions for the orchestrator-first backlog.

Single source of truth consumed by write_manifest.py, apply.py, verify.py.
No GitHub node IDs live here — only human-readable names; IDs are resolved
at runtime and cached outside the repo (/tmp/orchestraitor-board-cache/).
"""

ORG = "arbsec"
PROJECT_NUMBER = 1
REPO = "arbsec/orchestraitor"
SIBLING_REPO = "arbsec/arbitraitor"

# Budget defaults (values live in task bodies per plan decision D8; the spec
# carries mechanism classes only). Owner-adjustable at the board.
BUDGETS = (
    "attempts 3 | re-plan 2 | worker timeout 45m | concurrency 2 | "
    "stall 10m | backoff 10s*2^n capped at 5m | spend $10/day soft cap | "
    "run budget 4h"
)
BUDGET_BLOCK = "Budgets (owner-adjustable at the board): " + BUDGETS + "."

EPICS = [
    {
        "key": "E0",
        "title": "Epic: E0 — Self-improving bootstrap (thin vertical slice)",
        "priority": "P0",
        "target": "MVP",
        "risk": "High",
        "body": """## Summary

Minimal-depth end-to-end self-improvement loop, built first so the system starts
dogfooding its own backlog as fast as possible (owner intent: replace the
current external harness workflow). The slice runs: minimal GitHub board
provider (read + status-write) -> heuristic static routing only -> headless
mini-worker with the 4-tool minimal set (read files, search, mediated bash via
`arbitraitor_exec`, write via the transactional path) -> Restricted-sandbox
mediation + capability preflight -> worktree -> DCO commit -> scoped push ->
draft PR -> ONE-SHOT campaign pass -> simple loop runner `orc loop`
(cron-shaped; the full watch daemon arrives in E8).

E0 additionally carries: failure-driven Bug auto-file at attempts-exhaustion +
the blocking-fix exception (IS-14); MCP-early search/code-intelligence via
approved MCP servers (codegraph-style + codebase-memory-style; searches stay
mediated, servers untrusted per `docs/spec/20-harness-worker.md` §9.18.1); an
early task to register the ORCHESTRATOR SERVICE IDENTITY (GitHub App
`arbsec-agent`, machine identity per ledger D11); and the explicit scoping note
that adversarial review rides the EXISTING fresh-context review policy
(AGENTS.md) until the autonomous review-worker task lands under E7 (IS-6 leg).

Every E0 task body marks its thin-slice scope and names the deepening epic.

## Spec anchors

- `docs/spec/10-orchestrator.md` §9.35 (campaign orchestration: session-per-decision)
- `docs/spec/10-orchestrator.md` §9.36 (watch daemon — E0 ships the loop-runner subset)
- `docs/spec/10-orchestrator.md` §9.37 (agent issue reporting — failure-driven auto-file + blocking-fix exception)
- `docs/spec/10-orchestrator.md` §9.38 (MCP-early tool strategy)
- `docs/spec/10-orchestrator.md` §9.39 (coordinator decision tools — worker-facing subset)
- `docs/spec/10-orchestrator.md` §9.43 (kanban board abstraction — minimal provider subset)
- `docs/spec/30-model-routing.md` §9.45 (role-based model routing — heuristic table only)
- `docs/spec/40-arbitraitor-integration.md` §9.6 (sandbox integration), §16.2 (mandatory security feature workflow)
- `docs/spec/60-milestones.md` M1 (live self-referential loop on the shared board)

## Product ideal-state rows

- IS-1 (daemon polls board — E0 ships `orc loop`, not `orcd watch`)
- IS-2 (session-per-decision — one-shot pass)
- IS-3 (headless worker mode)
- IS-4 (mini-agent workers, 4-tool set)
- IS-5 (routing — static heuristic table only)
- IS-6 (self-improving loop; review rides existing policy; merge stays human-gated)
- IS-7 (workers inside the Arbitraitor boundary; capability preflight fail-closed)
- IS-14 (failure-driven Bug auto-file + blocking-fix exception)

## Focus

Focus: bootstrap the loop end-to-end at minimal depth; every trim deepens in E1-E10.

## Decomposition

11 leaf Tasks (native sub-issues). Nothing here is scheduled by this backlog;
E0 tasks land at Status=Ready for the future project-manager/loop to select.
""",
    },
    {
        "key": "E1",
        "title": "Epic: E1 — Board abstraction + providers + multi-org workspaces",
        "priority": "P1",
        "target": "MVP",
        "risk": "Medium",
        "body": """## Summary

Full-depth board layer: the `BoardProvider` trait (items, statuses, fields,
dependency edges, cross-references, search), a local sqlite provider for
CI/offline, and the GitHub Projects v2 provider with cached node IDs (outside
the repo) and an adaptive rate-limit tick. Ready-queue/stall/reconcile views
including the epic-focus predicate, assignee exclusion (service-identity set),
and dependency-cycle detection.

## Spec anchors

- `docs/spec/10-orchestrator.md` §9.41 (epic-focus scheduling policy — queue predicate)
- `docs/spec/10-orchestrator.md` §9.42 (multi-org workspaces and cross-project epics)
- `docs/spec/10-orchestrator.md` §9.43 (kanban board abstraction — full contract)
- `docs/spec/10-orchestrator.md` §9.40 (blocked-dependency semantics — edges are the DAG)
- `docs/spec/10-orchestrator.md` §9.36 (poll tick / reconcile semantics)
- `docs/spec/50-contracts-data.md` §21.3 (deterministic simulator — tests never hit the network)

## Product ideal-state rows

- IS-10 (pluggable board abstraction; GitHub Projects v2 first provider; local sqlite; GitHub not required)
- IS-13 (multi-org workspaces + cross-project epics)
- IS-12 (ready-queue predicate prerequisites: epic-focus + assignee exclusion)

## Focus

Focus: one canonical board provider per workspace; board-wins reconcile; never a mirrored second DAG.

## Decomposition

8 leaf Tasks (native sub-issues).
""",
    },
    {
        "key": "E2",
        "title": "Epic: E2 — Model routing + DecisionProvider + subscription awareness",
        "priority": "P1",
        "target": "MVP",
        "risk": "Medium",
        "body": """## Summary

Role-based model routing at full depth: role registry (built-in six roles +
custom), heuristic router with §9.19.2 precedence, persisted replayable routing
decision records; the `DecisionProvider` trait with a TypeSafe/jev
fixture-tested adapter (default-off until the license is allowlisted); and
subscription-aware routing (usage state, eligibility gate,
skipped-because-quota, all-exhausted stop).

## Spec anchors

- `docs/spec/30-model-routing.md` §9.45 (role-based model routing)
- `docs/spec/30-model-routing.md` §9.46 (subscription-aware routing)
- `docs/spec/30-model-routing.md` §9.19.2 (per-domain and per-role model routing precedence)
- `docs/spec/30-model-routing.md` §9.19.4-§9.19.6 (cost and subscription ledger, caps, budget scopes)
- `docs/spec/tech-stack.md` §17 (TypeSafe/jev license status: not yet allowlisted; default-off)

## Product ideal-state rows

- IS-5 (role-based routing; heuristic first; decision-model-backed selection pluggable; subscription-aware)

## Focus

Focus: heuristic table is the default and the fallback chain; no hard dependency on TypeSafe/jev anywhere in the workspace.

## Decomposition

6 leaf Tasks (native sub-issues).
""",
    },
    {
        "key": "E3",
        "title": "Epic: E3 — Coordinator decision tools",
        "priority": "P1",
        "target": "MVP",
        "risk": "Medium",
        "body": """## Summary

The agent-facing internal decision-tool surface: `board.query`,
`board.move` (guarded), `decision.record`, `router.consult`,
`worker.delegate` (scoped authority), `budget.check`, `capability.check` —
every invocation Arbitraitor-mediated like any other tool, never a silent
authority grant. Injection-boundary negatives are mandatory test surface.
Also carries the discretionary `report_issue` tool per IS-14.

## Spec anchors

- `docs/spec/10-orchestrator.md` §9.39 (coordinator decision tools)
- `docs/spec/10-orchestrator.md` §9.37(b) (discretionary report_issue, Triage-only)
- `docs/spec/10-orchestrator.md` §9.25.2 (short-lived, resource-scoped authority)
- `docs/spec/10-orchestrator.md` §9.25.3 (authority issuance and validation belong to Arbitraitor)
- `docs/spec/10-orchestrator.md` §9.35 (decision records)
- `docs/spec/60-milestones.md` §998 MVP-6 (built-in tool layer shape)

## Product ideal-state rows

- IS-9 (first-class internal decision-tool surface, Arbitraitor-mediated)
- IS-14(b) (discretionary report_issue capability)

## Focus

Focus: tools are tools — mediated, recorded, inspectable; worker.delegate scopes down, never inherits.

## Decomposition

8 leaf Tasks (native sub-issues).
""",
    },
    {
        "key": "E4",
        "title": "Epic: E4 — Mini-agent harness",
        "priority": "P1",
        "target": "MVP",
        "risk": "Medium",
        "body": """## Summary

The harness at full depth: headless one-shot worker (task in, structured
result + PR out, exit code) AND standalone interactive mode; the minimal
mediated toolset hardened with attempt bounds; and the built-in MCP PROXY
component per D10 — one endpoint fronting built-in tools, approved external
MCP servers, and Arbitraitor's own MCP server; routing/policy-surface only,
never a security authority.

## Spec anchors

- `docs/spec/20-harness-worker.md` §10.1 (integration modes)
- `docs/spec/20-harness-worker.md` §9.18.1 (MCP and tool drift — untrusted servers, advisory annotations)
- `docs/spec/10-orchestrator.md` §9.38 (MCP-early tool strategy and the built-in MCP proxy)
- `docs/spec/60-milestones.md` §998 MVP-6 (built-in coding tools)

## Product ideal-state rows

- IS-3 (standalone `orc` interactive AND spawned headless worker)
- IS-4 (mini-agent workers spawn cheaply with a minimal mediated toolset)

## Focus

Focus: one loop core, two entry modes; the proxy routes and presents policy, it never decides.

## Decomposition

4 leaf Tasks (native sub-issues).
""",
    },
    {
        "key": "E5",
        "title": "Epic: E5 — Arbitraitor worker mediation",
        "priority": "P1",
        "target": "MVP",
        "risk": "Critical",
        "body": """## Summary

Every worker runs inside the Arbitraitor security boundary: Restricted
sandbox wiring (`arbitraitor_sandbox::SandboxMode::Restricted` +
`compute_effective_controls` preflight, fail-closed), mediated bash via
`arbitraitor_exec::ExecutionContextBuilder`, capability preflight gate,
lease/cancellation onto the §9.24 lifecycle, receipts + approval-required
handling, and the `file_arbitraitor_gap` helper (missing capability -> open
arbsec/arbitraitor issue + `blocked:arbitraitor` + blocked wait). Two tasks are
blocked on upstream Arbitraitor gaps (headless ApprovalPrompt; ADR-0038
stable embedding API) and stay at Status=Backlog until those land.

**Risk=Critical for this epic: every task touches the Arbitraitor integration
boundary and requires human review before release** (workflow policy;
`docs/spec/40-arbitraitor-integration.md` §2.2, §16.2).

## Spec anchors

- `docs/spec/40-arbitraitor-integration.md` §9.6 (sandbox integration)
- `docs/spec/40-arbitraitor-integration.md` §9.8-§9.14 (policy, approval, command analysis, package gate, network, secrets, output quarantine)
- `docs/spec/40-arbitraitor-integration.md` §16.2 (mandatory security feature workflow)
- `docs/spec/40-arbitraitor-integration.md` §6.7 (Arbitraitor is the sole security authority; fail closed)
- `docs/spec/10-orchestrator.md` §9.24 (task and session lifecycle — leases, cancellation, recovery)

## Product ideal-state rows

- IS-7 (every worker inside the Arbitraitor boundary; missing capability files an upstream issue and blocks — never bypasses)

## Focus

Focus: fail closed or run in an explicitly labelled non-secure mode; a missing Arbitraitor capability is a filed gap, never a local workaround.

## Decomposition

8 leaf Tasks (native sub-issues); 2 are gap-linked (Status=Backlog,
`blocked:arbitraitor` label, cross-repo blockedBy edges to the two
arbsec/arbitraitor issues filed by this backlog).
""",
    },
    {
        "key": "E6",
        "title": "Epic: E6 — Task→worktree→draft-PR lifecycle",
        "priority": "P1",
        "target": "MVP",
        "risk": "Medium",
        "body": """## Summary

The delivery mechanics: worktree-per-task provisioning (trusted controller
owns Git metadata), DCO commits with bot attribution, scoped lease push
credential (short-lived, repo-scoped, minted via the GitHub App installation),
draft PR creation carrying spec anchors and evidence links, and PR-time
actual-diff Risk re-labelling (re-derive Risk from the real diff; Critical +
needs-human-review when the diff touches security-sensitive areas).

## Spec anchors

- `docs/spec/40-arbitraitor-integration.md` §6.2 (a worktree is not a sandbox)
- `docs/spec/20-harness-worker.md` §9.4 (workspace and Git controller)
- `docs/spec/10-orchestrator.md` §9.33.2 (task generation — task fields, spec traceability)
- `docs/spec/10-orchestrator.md` §9.25.2 (short-lived, resource-scoped authority — push credential)

## Product ideal-state rows

- IS-6 (loop produces PRs; merge remains human-gated)
- IS-3 (headless worker: task in, structured result + PR out)

## Focus

Focus: one independently testable, revertible concern per PR; the loop never merges its own PRs.

## Decomposition

5 leaf Tasks (native sub-issues).
""",
    },
    {
        "key": "E7",
        "title": "Epic: E7 — Campaign session runner",
        "priority": "P1",
        "target": "MVP",
        "risk": "Medium",
        "body": """## Summary

Full campaign semantics: session-per-decision (fresh, short-lived manager
session; one persisted decision record; exit), FocusState context assembly,
typed no-op reasons (empty-queue / all-blocked with blocked graph /
epic-exhausted), typed stop conditions (budget stops produce blocked or
needs-human, never silent), and the autonomous review-worker (IS-6 leg) that
drives the existing adversarial-review loop to convergence while merge stays
human-gated.

## Spec anchors

- `docs/spec/10-orchestrator.md` §9.35 (campaign orchestration: session-per-decision)
- `docs/spec/10-orchestrator.md` §9.33.3 (autonomous backlog execution — fresh contexts)
- `docs/spec/10-orchestrator.md` §9.33.4 (change-set and PR review loop)
- `docs/spec/10-orchestrator.md` §9.41 (epic-focus scheduling policy — FocusState input)

## Product ideal-state rows

- IS-2 (session-per-decision orchestration)
- IS-6 (self-improving loop: backlog -> selection -> worker -> PR -> adversarial review; merge human-gated)

## Focus

Focus: one decision record per session; no long-lived orchestrator conversation; crash-safe by construction.

## Decomposition

5 leaf Tasks (native sub-issues).
""",
    },
    {
        "key": "E8",
        "title": "Epic: E8 — Watch daemon + budgets + epic-focus controls",
        "priority": "P1",
        "target": "MVP",
        "risk": "Critical",
        "body": """## Summary

The always-running `orcd watch` daemon: adaptive poll tick (every tick a
reconcile pass), stall reaping + orphan detection, run + spend + subscription
budget enforcement, the `orc backlog pause/resume/cancel/retry/skip/
reprioritize/assign` controls, `orc epic focus/pause/resume` +
`auto_advance_epic` opt-in, and restart recovery from durable state.

**Guard- and budget-touching tasks in this epic are Risk=Critical with the
`needs-human-review` label** (only Critical forces human review on this
board): stall reaping, all three budget classes, the backlog controls, and
the epic-focus controls.

## Spec anchors

- `docs/spec/10-orchestrator.md` §9.36 (watch daemon)
- `docs/spec/10-orchestrator.md` §9.41 (epic-focus scheduling policy)
- `docs/spec/10-orchestrator.md` §9.24 (lifecycle — leases, TTLs, reaper, crash recovery)
- `docs/spec/10-orchestrator.md` §9.33.6 (durability and control — `orc backlog` controls)
- `docs/spec/30-model-routing.md` §9.46 (subscription-aware routing — budget class)

## Product ideal-state rows

- IS-1 (always-running daemon: polls, detects stalls, enforces budgets, kicks off work)
- IS-12 (epic-focus scheduling: one active epic, bug preemption, focus/pause/resume, needs-human on exhausted/blocked)

## Focus

Focus: the daemon executes and supervises decisions; it owns none. Budget stops are visible states, never silent skips.

## Decomposition

7 leaf Tasks (native sub-issues).
""",
    },
    {
        "key": "E9",
        "title": "Epic: E9 — Operator chat",
        "priority": "P2",
        "target": "MVP",
        "risk": "Low",
        "body": """## Summary

`orc chat`: the operator's conversational surface. Progress reads are
read-only summaries of durable state stamped with last-synced caveats;
drafting lands new Epics/Features/Tasks at Status=Triage (never scheduled);
focus steering maps to the same guarded controls as `orc epic
focus/pause/resume`; chat output carries no execution authority
(authority-refusal negatives required).

## Spec anchors

- `docs/spec/10-orchestrator.md` §9.44 (operator chat mode)
- `docs/spec/10-orchestrator.md` §9.41 (focus controls — steering target)

## Product ideal-state rows

- IS-11 (operator chat mode: progress from durable state; Triage drafting only)

## Focus

Focus: reads are views, drafts are Triage, steering is the operator's command through the normal control path.

## Decomposition

4 leaf Tasks (native sub-issues).
""",
    },
    {
        "key": "E10",
        "title": "Epic: E10 — E2E + adversarial + CI hardening",
        "priority": "P1",
        "target": "MVP",
        "risk": "High",
        "body": """## Summary

Proof layer: the hermetic self-referential loop smoke (board -> manager ->
worker -> PR -> adversarial review -> human-gated merge, deterministic
simulator + local board, no live network); injection/escape, budget/stall,
and merge-safety adversarial suites including the guardrail-weakening
fixture; and parity-gate expansion to the new orchestration crates.

## Spec anchors

- `docs/spec/50-contracts-data.md` §21.3 (deterministic simulator)
- `docs/spec/50-contracts-data.md` §21.4 (adversarial end-to-end tests)
- `docs/spec/60-milestones.md` M0 (board abstraction, mediated mini-worker, hermetic simulator E2E)
- `docs/spec/tech-stack.md` §15 (parity gate)

## Product ideal-state rows

- IS-6 (loop proven end-to-end; merge safety asserted)
- IS-7 (containment proven by negative tests — forbidden effects asserted not to occur)

## Focus

Focus: never claim a sandbox test passed merely because an error occurred — assert the forbidden effect did not happen.

## Decomposition

5 leaf Tasks (native sub-issues).
""",
    },
    {
        "key": "ICE",
        "title": "Epic: ICE — TUI dashboard (C7 deferral, Icebox)",
        "priority": "P3",
        "target": "Icebox",
        "risk": "Low",
        "body": """## Summary

Placeholder epic recording the C7 deferral visibly: the
agent-orchestrator-style TUI dashboard (derived Kanban over durable facts) is
deliberately deferred until the loop is done ("similar UI when it is done" —
owner). Target=Icebox: never scheduled during MVP; kept on the board so the
deferral is a visible decision, not a silent drop.

## Spec anchors

- `docs/spec/10-orchestrator.md` §9.44 (operator surfaces — the TUI exposes equivalent views later)
- Research ledger component C7 (deferred, post-initial-goal)

## Product ideal-state rows

- None (deferred capability; no IS row)

## Focus

Focus: do not schedule; revisit only after the self-referential loop milestone (M1) is complete.

## Decomposition

1 placeholder leaf Task (native sub-issue).
""",
    },
]
