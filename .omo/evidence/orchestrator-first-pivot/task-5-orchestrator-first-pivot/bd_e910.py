#!/usr/bin/env python3
"""E9 + E10 + ICE leaf tasks: operator chat, E2E/adversarial/CI hardening, icebox placeholder."""

TASKS = [
    {
        "id": "E9-T1",
        "epic": "E9",
        "title": "Task: chat-progress — `orc chat` progress reads from durable state (last-synced stamps)",
        "risk": "Low",
        "estimate": 3,
        "labels": [],
        "blocked_by": [],
        "body": """## Objective

`orc chat` progress reads: conversational summaries of durable state —
board state, campaign decisions, run state, budgets, blocked graph — read
from their durable homes, never from chat-context memory. Every answer is a
read-only view stamped with the same last-synced caveats as every other
read.

## References

- `docs/spec/10-orchestrator.md` §9.44 (operator chat mode — progress reads)
- `docs/spec/10-orchestrator.md` §9.43 (durable homes + last-synced)
- `docs/spec/10-orchestrator.md` §9.35 (decision records chat can summarize)

## Acceptance criteria

- [ ] Progress questions answer from board/run state with last-synced stamps visible in the answer
- [ ] No chat-context memory: a fresh chat session answers identically from durable state (test)
- [ ] `cargo nextest run --workspace` green including chat-read unit tests (simulator-backed)

## QA scenarios

- Happy: "what's the loop doing?" -> summary of active epic, in-flight tasks, budgets, blocked graph, each stamped; evidence `.omo/evidence/<task-slug>/chat-progress.txt`
- Failure: stale board read -> the answer carries the older last-synced stamp (assert visible staleness, never fabricated freshness); evidence `.omo/evidence/<task-slug>/chat-stale.txt`

## Non-goals

No drafting (E9-T2); no steering (E9-T3); no TUI.

## Security impact

Read-only; no authority.

## Testing requirements

Determinism-from-durable-state tests; staleness-visibility tests.

## Documentation impact

`docs/cli/` chat reference; CHANGELOG `[Unreleased]`.

## Dependencies

None upstream (consumes E1/E7/E8 state at runtime).

## Rollback implications

Additive surface.
""",
    },
    {
        "id": "E9-T2",
        "epic": "E9",
        "title": "Task: chat-drafting — Triage drafting (Epics/Features/Tasks land on the board, never the schedule)",
        "risk": "Medium",
        "estimate": 3,
        "labels": [],
        "blocked_by": ["E9-T1"],
        "body": """## Objective

Chat drafting: the operator can draft new Epics, Features, and Tasks through
chat; drafts are filed at Status=Triage — the human/PM triage gate — never
silently scheduled. Drafting is not decomposition: decomposition into leaf
sub-issues follows the workflow policy, and the PM selection gate stays
intact.

## References

- `docs/spec/10-orchestrator.md` §9.44 (drafting lands on the board, never the schedule)
- `.agents/project/orchestraitor-workflow.md` (triage gate; decomposition policy)

## Acceptance criteria

- [ ] Chat-drafted items land at Status=Triage with structured bodies (never Ready, never scheduled)
- [ ] Draft bodies are marker-wrapped untrusted content (model-generated text is data)
- [ ] `cargo nextest run --workspace` green including drafting unit tests

## QA scenarios

- Happy: "draft a task for X" -> Triage item with structured body; queue excludes it; evidence `.omo/evidence/<task-slug>/chat-draft.txt`
- Failure: a draft attempting Status=Ready -> refused (typed); injection payload in the draft body -> quoted data only; evidence `.omo/evidence/<task-slug>/chat-draft-refusals.txt`

## Non-goals

No auto-decomposition; no scheduling; no field writes beyond Triage filing.

## Security impact

Untrusted-content boundary (model-generated bodies).

## Testing requirements

Refusal + injection negatives (reuse E3-T7 corpus).

## Documentation impact

Chat reference; CHANGELOG `[Unreleased]`.

## Dependencies

- Blocked by E9-T1 (chat shell).

## Rollback implications

Drafts are Triage items — closable.
""",
    },
    {
        "id": "E9-T3",
        "epic": "E9",
        "title": "Task: chat-steering — focus steering via the guarded controls",
        "risk": "Medium",
        "estimate": 3,
        "labels": [],
        "blocked_by": ["E9-T1"],
        "body": """## Objective

Chat steering: focus switch, pause, resume requested through chat map to
the same guarded controls as `orc epic focus/pause/resume` (E8-T6) — the
operator's instruction is the command and executes through the normal
control path; model-generated text never mutates state on its own.

## References

- `docs/spec/10-orchestrator.md` §9.44 (steering maps to the guarded controls)
- `docs/spec/10-orchestrator.md` §9.41 (the controls being mapped to)

## Acceptance criteria

- [ ] Steering intents execute via the E8-T6 control path (same guards, same records) — not a parallel write path
- [ ] Model-generated text outside an operator-confirmed steering intent mutates nothing (test)
- [ ] `cargo nextest run --workspace` green including steering unit tests

## QA scenarios

- Happy: "focus E1" -> the same effect as `orc epic focus E1` (assert identical control-path record); evidence `.omo/evidence/<task-slug>/chat-steer.txt`
- Failure: model output containing "focus E2" without an operator instruction -> no state change (assert); evidence `.omo/evidence/<task-slug>/chat-no-unauthorized-steer.txt`

## Non-goals

No new controls; no autonomous steering.

## Security impact

Authority routing: operator principal, not model output.

## Testing requirements

Path-equivalence tests; unauthorized-steering negatives.

## Documentation impact

Chat reference; CHANGELOG `[Unreleased]`.

## Dependencies

- Blocked by E9-T1 (chat shell); consumes E8-T6 controls at runtime.

## Rollback implications

Steering rides guarded controls — revertible.
""",
    },
    {
        "id": "E9-T4",
        "epic": "E9",
        "title": "Task: chat-authority-negatives — authority-refusal negative suite (chat carries no execution authority)",
        "risk": "High",
        "estimate": 3,
        "labels": [],
        "blocked_by": ["E9-T2", "E9-T3"],
        "body": """## Objective

The adversarial suite proving chat's distinct authority profile: progress
queries are read-only; drafting writes Triage-only; chat output carries no
execution authority. Injection payloads addressed to the chat surface
(instructions to spawn workers, merge PRs, bypass budgets, self-escalate)
must all be refused or treated as data.

## References

- `docs/spec/10-orchestrator.md` §9.44 (distinct authority profile)
- `docs/spec/50-contracts-data.md` §21.4 (adversarial tests)
- `docs/spec/40-arbitraitor-integration.md` §6.1 (agent is always untrusted)

## Acceptance criteria

- [ ] A payload corpus (spawn/merge/bypass/self-assign/self-promote instructions embedded in chat turns) runs against the chat surface
- [ ] Each case asserts the forbidden effect did not occur (no spawn, no merge, no budget change, no Ready promotion)
- [ ] `cargo nextest run --workspace` green; the suite runs in CI

## QA scenarios

- Happy: all payloads refused/neutralized with typed responses; evidence `.omo/evidence/<task-slug>/chat-authority-suite.txt`
- Failure (proof the suite bites): a weakened chat fixture that executes an embedded instruction is caught -> test fails; evidence `.omo/evidence/<task-slug>/chat-suite-detects-weakness.txt`

## Non-goals

No red-teaming infrastructure beyond the corpus.

## Security impact

This IS the security test surface for the chat boundary.

## Testing requirements

The suite; canary detection case.

## Documentation impact

Test-surface docs; CHANGELOG `[Unreleased]`.

## Dependencies

- Blocked by E9-T2, E9-T3 (the surfaces exist first).

## Rollback implications

Test-only; additive.
""",
    },
    # ---------------- E10 ----------------
    {
        "id": "E10-T1",
        "epic": "E10",
        "title": "Task: e2e-loop-smoke — hermetic self-referential loop smoke (simulator + local board)",
        "risk": "Medium",
        "estimate": 8,
        "labels": [],
        "blocked_by": [],
        "body": """## Objective

The hermetic E2E smoke per M0/§21.3: board -> manager selection -> mediated
mini-worker -> pull request -> adversarial review -> human-gated merge,
running entirely on the deterministic simulator + local sqlite board with no
live network. This is the M0 exit-criteria proof and the regression backbone
for the whole loop.

## References

- `docs/spec/60-milestones.md` M0 (board abstraction, mediated mini-worker, hermetic simulator E2E)
- `docs/spec/50-contracts-data.md` §21.3 (deterministic simulator)
- `docs/spec/60-milestones.md` §998 MVP-8 (headless and CI support)

## Acceptance criteria

- [ ] A single CI job runs the full loop hermetically: seeded sqlite board -> campaign session -> mediated worker (simulator provider) -> draft PR (fixture remote) -> review trigger -> human-gate assertion
- [ ] Zero live network in the test process (assert via harness sandbox)
- [ ] Reproducible from recorded fixtures (same seed -> same run)
- [ ] `cargo nextest run --workspace` green; the smoke runs in CI on every PR

## QA scenarios

- Happy: full pass with all state transitions visible on the local board; evidence `.omo/evidence/<task-slug>/loop-smoke.txt`
- Failure: any unmediated tool call, network access, or merge-without-human in the run -> the smoke fails (assert, not just error-observe); evidence `.omo/evidence/<task-slug>/loop-smoke-guards.txt`

## Non-goals

No live GitHub run (that is M1, exercised with evidence captured manually); no performance benchmarks.

## Security impact

The smoke asserts the containment invariants hold end-to-end.

## Testing requirements

The smoke itself + guard assertions inside it.

## Documentation impact

CI docs; CHANGELOG `[Unreleased]`.

## Dependencies

None upstream (consumes E0-E7 components at runtime).

## Rollback implications

Test-only; additive.
""",
    },
    {
        "id": "E10-T2",
        "epic": "E10",
        "title": "Task: e2e-injection-escape — injection/escape adversarial suite",
        "risk": "High",
        "estimate": 5,
        "labels": [],
        "blocked_by": ["E10-T1"],
        "body": """## Objective

The injection/escape adversarial suite per §21.4: prompt injection via board
content and tool results, marker-escape attempts, cross-principal content,
and untrusted-output escape attempts against the full loop (not just single
tools — extends E3-T7 to end-to-end).

## References

- `docs/spec/50-contracts-data.md` §21.4 (adversarial end-to-end tests)
- `docs/spec/10-orchestrator.md` §9.39 (injection-boundary negatives)
- `docs/spec/40-arbitraitor-integration.md` §7.3 (primary attack classes)

## Acceptance criteria

- [ ] End-to-end fixtures inject payloads at every untrusted-content ingress (board bodies, tool results, MCP results, PR text)
- [ ] Each case asserts the forbidden effect did not occur (no command execution, no authority grant, no field influence)
- [ ] `cargo nextest run --workspace` green; the suite runs in CI

## QA scenarios

- Happy: all payloads neutralized across the loop; evidence `.omo/evidence/<task-slug>/injection-e2e.txt`
- Failure (proof the suite bites): a weakened-loop fixture executing one payload is caught; evidence `.omo/evidence/<task-slug>/injection-e2e-detects.txt`

## Non-goals

No live-model red-teaming (simulator payloads only).

## Security impact

The end-to-end security test surface.

## Testing requirements

The suite; canary detection case.

## Documentation impact

Test-surface docs; CHANGELOG `[Unreleased]`.

## Dependencies

- Blocked by E10-T1 (the loop smoke is the harness).

## Rollback implications

Test-only; additive.
""",
    },
    {
        "id": "E10-T3",
        "epic": "E10",
        "title": "Task: e2e-budget-stall — budget/stall adversarial suite (incl. guardrail-weakening fixture)",
        "risk": "High",
        "estimate": 5,
        "labels": [],
        "blocked_by": ["E10-T1"],
        "body": """## Objective

The budget/stall adversarial suite per §21.4/§9.36: budget exhaustion
produces needs-human (never silent success), stall reaping works under
adversarial conditions (workers that heartbeat-then-hang, clock skew), and
the guardrail-weakening fixture — a change that attempts to disable or
loosen a guard (budget, stall, concurrency, attempt bound) is detected and
fails the gate.

## References

- `docs/spec/50-contracts-data.md` §21.4 (adversarial tests)
- `docs/spec/10-orchestrator.md` §9.36 (budget enforcement; stall detection)
- `docs/spec/10-orchestrator.md` §9.33.5 (failure classes)

## Acceptance criteria

- [ ] Exhaustion fixtures (attempts, run, spend, subscription) each produce visible stops — never silent success, never weakened retry
- [ ] Adversarial stall fixtures (heartbeat-then-hang, rapid re-spawn) are reaped correctly
- [ ] The guardrail-weakening fixture fails CI when a guard is weakened (canary proof)
- [ ] `cargo nextest run --workspace` green; the suite runs in CI

## QA scenarios

- Happy: all adversarial fixtures handled with visible states; evidence `.omo/evidence/<task-slug>/budget-stall-suite.txt`
- Failure (proof the suite bites): deliberately weaken one guard in a fixture build -> CI fails; evidence `.omo/evidence/<task-slug>/guardrail-canary.txt`

## Non-goals

No chaos infrastructure beyond fixtures.

## Security impact

The loop-safety test surface (guards are what keep the loop bounded).

## Testing requirements

The suite; the canary.

## Documentation impact

Test-surface docs; CHANGELOG `[Unreleased]`.

## Dependencies

- Blocked by E10-T1 (the loop smoke is the harness).

## Rollback implications

Test-only; additive.
""",
    },
    {
        "id": "E10-T4",
        "epic": "E10",
        "title": "Task: e2e-merge-safety — merge-safety suite (the loop never merges its own PRs)",
        "risk": "High",
        "estimate": 3,
        "labels": [],
        "blocked_by": ["E10-T1"],
        "body": """## Objective

The merge-safety suite: prove the loop can never merge its own pull
requests — the human gate is structural, not configurational. Fixtures
attempt every merge path (direct merge, admin merge, review-then-merge by
the loop's own identities, auto-merge labels) and each must be refused.

## References

- `docs/spec/10-orchestrator.md` §9.33.4 (review loop; merge human-gated)
- `docs/spec/60-milestones.md` M1 exit criteria ("The loop never merges its own pull requests")
- AGENTS.md (merge invariants)

## Acceptance criteria

- [ ] Fixtures attempt merge via every available path under the loop's identities; all refused (assert no merged PR exists after)
- [ ] The human gate is asserted structurally (no code path grants merge authority to loop identities)
- [ ] `cargo nextest run --workspace` green; the suite runs in CI

## QA scenarios

- Happy: all merge attempts refused; PRs remain open for the human; evidence `.omo/evidence/<task-slug>/merge-safety.txt`
- Failure (proof the suite bites): a fixture granting the loop a merge path is caught -> test fails; evidence `.omo/evidence/<task-slug>/merge-safety-canary.txt`

## Non-goals

No changes to the repo's merge rulesets (they already enforce this for humans).

## Security impact

The self-modification safety invariant (the loop must not approve its own work).

## Testing requirements

The suite; the canary.

## Documentation impact

Test-surface docs; CHANGELOG `[Unreleased]`.

## Dependencies

- Blocked by E10-T1 (the loop smoke is the harness).

## Rollback implications

Test-only; additive.
""",
    },
    {
        "id": "E10-T5",
        "epic": "E10",
        "title": "Task: ci-parity-gate — parity-gate expansion to the new orchestration crates",
        "risk": "Low",
        "estimate": 3,
        "labels": [],
        "blocked_by": [],
        "body": """## Objective

Extend the parity gate (tech-stack §15) to every new orchestration crate as
they land: `cargo fmt --check`, `cargo clippy --workspace --all-targets
--all-features -- -D warnings`, `cargo check --locked`, `cargo nextest run`,
`rumdl check .`, `cargo deny check`, `cargo audit` — all green in CI for the
full workspace including the new crates, with the lint policy
(`#![forbid(unsafe_code)]`, `#![deny(unwrap_used, expect_used, ...)]`)
applied from each crate's first commit.

## References

- `docs/spec/tech-stack.md` §15 (parity gate — the source of truth)
- AGENTS.md (Rust conventions; pre-PR gate)
- `.github/workflows/README.md` (check-name registry)

## Acceptance criteria

- [ ] Every new crate enters the workspace with the lint policy attributes from its first commit (CI-enforced)
- [ ] The full parity gate runs on every PR and is green
- [ ] Check names registered per `.github/workflows/README.md` conventions

## QA scenarios

- Happy: full gate green on a PR touching a new crate; evidence `.omo/evidence/<task-slug>/parity-green.txt`
- Failure: a new crate missing the lint attributes -> CI fails with the missing-attribute named; evidence `.omo/evidence/<task-slug>/parity-lint-caught.txt`

## Non-goals

No new checks beyond the parity gate; no coverage targets yet (workflow policy defers until wiring).

## Security impact

Supply-chain + lint hygiene (deny/audit are part of the gate).

## Testing requirements

The gate itself; a lint-attribute canary.

## Documentation impact

CI docs; CHANGELOG `[Unreleased]`.

## Dependencies

None upstream (rides each crate-landing PR).

## Rollback implications

CI config; revertible.
""",
    },
    # ---------------- ICE (Icebox placeholder) ----------------
    {
        "id": "ICE-T1",
        "epic": "ICE",
        "title": "Task: icebox-tui-dashboard — TUI dashboard for orchestration state (DEFERRED — C7)",
        "risk": "Low",
        "estimate": 8,
        "labels": [],
        "blocked_by": [],
        "status": "Backlog",
        "target": "Icebox",
        "body": """## Objective

Placeholder recording the C7 deferral: an agent-orchestrator-style TUI
dashboard (derived Kanban over durable facts — display status DERIVED at
read time, never stored) for orchestration state. Deliberately deferred
until the self-referential loop milestone (M1) is complete ("similar UI
when it is done" — owner). Target=Icebox: never scheduled during MVP.

## References

- `docs/spec/10-orchestrator.md` §9.44 (operator surfaces — TUI exposes equivalent views later)
- Research ledger component C7 (deferred; AO derived-status pattern as reference)

## Acceptance criteria

- [ ] None (deferred). This item exists so the deferral is a visible board decision, not a silent drop. Revisit after M1.

## QA scenarios

- None (deferred).

## Non-goals

Everything — this is a placeholder.

## Security impact

None (not built).

## Testing requirements

None.

## Documentation impact

None while deferred.

## Dependencies

None.

## Rollback implications

N/A.
""",
    },
]
