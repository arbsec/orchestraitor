#!/usr/bin/env python3
"""E3 leaf tasks: coordinator decision tools."""

TASKS = [
    {
        "id": "E3-T1",
        "epic": "E3",
        "title": "Task: tools-board-query — `board.query` decision tool",
        "risk": "Low",
        "estimate": 3,
        "labels": [],
        "blocked_by": [],
        "body": """## Objective

The `board.query` coordinator decision tool: read board state — items,
statuses, fields, dependency edges, blocked graph — through the
`BoardProvider` contract, exposed on the MVP-6-style built-in tool layer for
orchestration rather than file operations.

## References

- `docs/spec/10-orchestrator.md` §9.39 (coordinator decision tools — board.query)
- `docs/spec/10-orchestrator.md` §9.43 (board abstraction the tool reads)
- `docs/spec/60-milestones.md` §998 MVP-6 (tool-layer shape)

## Acceptance criteria

- [ ] `board.query` accepts structured filters (status, type, epic, fields) and returns typed results (no raw provider JSON passthrough)
- [ ] The blocked-graph query returns the transitive blocked set for an item
- [ ] Tool invocation is recorded in the event store with the §9.25.1 delegation chain
- [ ] `cargo nextest run --workspace` green including tool unit tests (sqlite board fixtures)

## QA scenarios

- Happy: query by epic + status returns typed items incl. dependency edges; evidence `.omo/evidence/<task-slug>/board-query.txt`
- Failure: injection fixture (filter value containing instructions) is treated as data — results unchanged, no command execution; evidence `.omo/evidence/<task-slug>/board-query-injection.txt`

## Non-goals

No writes (E3-T2); no policy decisions.

## Security impact

Read-only; untrusted board content stays data.

## Testing requirements

Unit tests + injection negative (shared harness with E3-T7).

## Documentation impact

Tool reference docs; CHANGELOG `[Unreleased]`.

## Dependencies

None (consumes E1 contract at runtime).

## Rollback implications

Additive tool; disable via config.
""",
    },
    {
        "id": "E3-T2",
        "epic": "E3",
        "title": "Task: tools-board-move — `board.move` guarded board transitions",
        "risk": "Medium",
        "estimate": 3,
        "labels": [],
        "blocked_by": ["E3-T1"],
        "body": """## Objective

The `board.move` decision tool: guarded board transitions (status/field
writes) — validated against workflow-policy rules, lease-checked, and
reconcile-visible. Never a raw provider write: transitions the provider or
workflow policy rejects are refused with typed reasons.

## References

- `docs/spec/10-orchestrator.md` §9.39 (board.move — guarded, lease-checked)
- `.agents/project/orchestraitor-workflow.md` (transition rules the guard enforces)

## Acceptance criteria

- [ ] Valid transitions apply; invalid ones (e.g. moving an item with an unresolved blocker into In Progress) are refused with typed reasons
- [ ] Lease check: moving an item another session holds a lease on is refused
- [ ] Every applied transition is reconcile-visible (board-wins on next tick)
- [ ] `cargo nextest run --workspace` green including guard unit tests

## QA scenarios

- Happy: Ready -> In Progress for an owned, unblocked item applies and round-trips; evidence `.omo/evidence/<task-slug>/move-happy.txt`
- Failure: policy-invalid + lease-conflict fixtures -> typed refusals, board unchanged; evidence `.omo/evidence/<task-slug>/move-refused.txt`

## Non-goals

No edge writes; no field writes beyond status-class fields.

## Security impact

Guard correctness prevents out-of-policy state changes (scheduling safety).

## Testing requirements

Guard unit tests; refusal negatives.

## Documentation impact

Tool reference docs; CHANGELOG `[Unreleased]`.

## Dependencies

- Blocked by E3-T1 (shared tool surface).

## Rollback implications

Transitions are board writes — revertible on the board.
""",
    },
    {
        "id": "E3-T3",
        "epic": "E3",
        "title": "Task: tools-decision-record — `decision.record` (append-only, replayable)",
        "risk": "Low",
        "estimate": 2,
        "labels": [],
        "blocked_by": [],
        "body": """## Objective

The `decision.record` decision tool: persist a decision record with rationale
per the §9.35 shape (kind, selected task, role, model+provider, worker
arguments, rationale, alternatives considered) — append-only and replayable.

## References

- `docs/spec/10-orchestrator.md` §9.39 (decision.record)
- `docs/spec/10-orchestrator.md` §9.35 (decision record shape)

## Acceptance criteria

- [ ] Records are append-only (no update/delete path on the tool)
- [ ] The §9.35 fields are validated on write; malformed records are refused
- [ ] `cargo nextest run --workspace` green including record unit tests

## QA scenarios

- Happy: a spawn decision persists with all fields; replay reads it back identically; evidence `.omo/evidence/<task-slug>/decision-record.txt`
- Failure: attempt to append a record missing required fields -> typed refusal; evidence `.omo/evidence/<task-slug>/decision-refused.txt`

## Non-goals

No analytics; no cross-session mutation.

## Security impact

Records must not contain secrets (redaction rules).

## Testing requirements

Schema validation + append-only negatives.

## Documentation impact

Tool reference docs; CHANGELOG `[Unreleased]`.

## Dependencies

None.

## Rollback implications

Append-only local state.
""",
    },
    {
        "id": "E3-T4",
        "epic": "E3",
        "title": "Task: tools-router-consult — `router.consult` decision tool",
        "risk": "Low",
        "estimate": 2,
        "labels": [],
        "blocked_by": [],
        "body": """## Objective

The `router.consult` decision tool: ask the model router for a
`(provider, model)` resolution with alternatives — resolving through the
role registry and returning the same record shape the router persists,
including skip reasons.

## References

- `docs/spec/10-orchestrator.md` §9.39 (router.consult)
- `docs/spec/30-model-routing.md` §9.45 (role resolution; decision records)

## Acceptance criteria

- [ ] Consult returns `(provider, model, routing_reason)` + alternatives with skip reasons
- [ ] Consulting does not mutate routing state (read-only resolution)
- [ ] `cargo nextest run --workspace` green including consult unit tests

## QA scenarios

- Happy: consult for each built-in role returns resolutions + alternatives; evidence `.omo/evidence/<task-slug>/router-consult.txt`
- Failure: unresolvable role -> typed failure with the missing-config key named; evidence `.omo/evidence/<task-slug>/consult-unresolvable.txt`

## Non-goals

No model selection by the worker (control-plane only).

## Security impact

None (read-only).

## Testing requirements

Unit tests; unresolvable negative.

## Documentation impact

Tool reference docs; CHANGELOG `[Unreleased]`.

## Dependencies

None (consumes E2 router at runtime).

## Rollback implications

Additive tool.
""",
    },
    {
        "id": "E3-T5",
        "epic": "E3",
        "title": "Task: tools-worker-delegate — `worker.delegate` with scoped authority (§9.25.2)",
        "risk": "Critical",
        "estimate": 5,
        "labels": ["needs-human-review"],
        "blocked_by": [],
        "body": """## Objective

The `worker.delegate` decision tool: spawn a worker with scoped authority —
explicit capability requests, lease-aligned expiry, never the parent's full
authority. Every invocation is assembled into a `PlanContext` and authorized
via `arbitraitor_mcp::ApprovalTokenIssuer` where security-sensitive
(§9.25.3), evaluated by Arbitraitor's `PolicyEngine`, recorded with the
§9.25.1 delegation chain.

## References

- `docs/spec/10-orchestrator.md` §9.39 (worker.delegate — scoped authority)
- `docs/spec/10-orchestrator.md` §9.25.2 (short-lived, resource-scoped authority)
- `docs/spec/10-orchestrator.md` §9.25.3 (authority issuance and validation belong to Arbitraitor)
- `docs/spec/tech-stack.md` §2.2 (real identifiers: `PlanContext`, `ApprovalTokenIssuer`, `PolicyEngine`)

## Acceptance criteria

- [ ] Delegation requires explicit capability requests; a request without capabilities is refused
- [ ] Granted authority expires with the lease (expiry test with virtual clock); the parent's authority is never inherited implicitly
- [ ] Every delegation is recorded (delegation chain + decision record)
- [ ] `cargo nextest run --workspace` green including delegation unit tests

## QA scenarios

- Happy: delegate with explicit capabilities -> worker runs with exactly those (assert via receipt scope); evidence `.omo/evidence/<task-slug>/delegate-scoped.txt`
- Failure: expired-authority use is refused by the mediation boundary (assert the forbidden effect did not occur); a delegation attempting parent-scope capabilities is refused; evidence `.omo/evidence/<task-slug>/delegate-refusals.txt`

## Non-goals

No authority minting in Orchestraitor (Arbitraitor's job); no human-approval UI.

## Security impact

Capability-issuance wiring at the trust boundary: Risk=Critical, needs-human-review.

## Testing requirements

Adversarial negatives: scope-escalation attempts, expiry races.

## Documentation impact

Tool reference docs; CHANGELOG `[Unreleased]`.

## Dependencies

None upstream in E3 (consumes E5 mediation at runtime).

## Rollback implications

Additive tool; disable via config.
""",
    },
    {
        "id": "E3-T6",
        "epic": "E3",
        "title": "Task: tools-budget-capability — `budget.check` + `capability.check` decision tools",
        "risk": "Medium",
        "estimate": 3,
        "labels": [],
        "blocked_by": [],
        "body": """## Objective

The `budget.check` decision tool (query spend, run/time, and subscription
budget state per §9.36 classes) and the `capability.check` decision tool
(query Arbitraitor's capability report for a requested operation shape —
reporting Arbitraitor's answer, never creating one).

## References

- `docs/spec/10-orchestrator.md` §9.39 (budget.check, capability.check)
- `docs/spec/10-orchestrator.md` §9.36 (budget classes)
- `docs/spec/40-arbitraitor-integration.md` §6.7 (capability reporting — Arbitraitor's answer)

## Acceptance criteria

- [ ] `budget.check` returns the three budget classes' current state (spend, run/time, subscription) with remaining values
- [ ] `capability.check` returns Arbitraitor's capability report for a requested operation shape, verbatim (no local interpretation)
- [ ] `cargo nextest run --workspace` green including both tools' unit tests

## QA scenarios

- Happy: budget.check against a fixture ledger returns per-class state; capability.check returns the probe matrix for a bash-exec shape; evidence `.omo/evidence/<task-slug>/check-tools.txt`
- Failure: capability.check when Arbitraitor reports a missing control -> the report says so (fail-closed information, no local fallback answer); evidence `.omo/evidence/<task-slug>/capability-missing.txt`

## Non-goals

No enforcement (E8 daemon enforces); no probe implementation (Arbitraitor's).

## Security impact

Reporting layer only; must not fabricate capability answers.

## Testing requirements

Unit tests; no-fabrication negative.

## Documentation impact

Tool reference docs; CHANGELOG `[Unreleased]`.

## Dependencies

None.

## Rollback implications

Additive tools.
""",
    },
    {
        "id": "E3-T7",
        "epic": "E3",
        "title": "Task: tools-injection-negatives — injection-boundary negative test suite for decision tools",
        "risk": "High",
        "estimate": 3,
        "labels": [],
        "blocked_by": ["E3-T1"],
        "body": """## Objective

The mandatory adversarial test surface for the decision tools: a tool
argument containing instructions, marker-escape attempts, or content
addressed to a different principal MUST be treated as data — quoted,
sanitized, never executed. Shared fixture library covering all §9.39 tools.

## References

- `docs/spec/10-orchestrator.md` §9.39 (injection-boundary negatives are mandatory test surface)
- `docs/spec/40-arbitraitor-integration.md` §6.1 (agent is always untrusted)
- `docs/spec/50-contracts-data.md` §21.4 (adversarial tests)

## Acceptance criteria

- [ ] A fixture corpus of injection payloads (instructions, marker escapes, cross-principal content) runs against every decision tool
- [ ] Each case asserts the forbidden effect did not occur (no command execution, no field influence beyond typed inputs, no authority grant)
- [ ] `cargo nextest run --workspace` green; the suite runs in CI

## QA scenarios

- Happy: all payloads neutralized (results typed, no execution); evidence `.omo/evidence/<task-slug>/injection-suite.txt`
- Failure (proof the suite bites): a deliberately weakened tool fixture (one that interpolates arguments) is caught by the suite -> test fails; evidence `.omo/evidence/<task-slug>/suite-detects-weakness.txt`

## Non-goals

No fuzzing infrastructure beyond the corpus; no live-model tests.

## Security impact

This IS the security test surface for the tool boundary.

## Testing requirements

The suite itself; a canary case proving detection (guardrail-weakening fixture pattern).

## Documentation impact

Test-surface docs; CHANGELOG `[Unreleased]`.

## Dependencies

- Blocked by E3-T1 (first tool exists; suite extends to all).

## Rollback implications

Test-only; additive.
""",
    },
    {
        "id": "E3-T8",
        "epic": "E3",
        "title": "Task: tools-report-issue — discretionary `report_issue` tool (IS-14b, Triage-only)",
        "risk": "Low",
        "estimate": 3,
        "labels": [],
        "blocked_by": [],
        "body": """## Objective

The discretionary `report_issue` coordinator tool per IS-14/§9.37(b): agents
report issues noticed but not task-fatal — structured content (title, class,
description), evidence links, affected paths. Files at Status=Triage ONLY;
never auto-scheduled, never promoted to Ready by the reporting path, never
assigned to its reporter. Agent-authored bodies are marker-wrapped untrusted
content.

## References

- `docs/spec/10-orchestrator.md` §9.37(b) (discretionary reporting)
- `docs/spec/10-orchestrator.md` §9.37(d) (untrusted content)
- `docs/spec/10-orchestrator.md` §9.39 (tool surface)

## Acceptance criteria

- [ ] `report_issue` files an issue at Status=Triage with structured fields; the reporter is not the assignee
- [ ] The filed issue is never promoted to Ready by the reporting path (assert queue exclusion)
- [ ] Bodies are marker-wrapped; injection corpus cases pass (reuses E3-T7 fixtures)
- [ ] `cargo nextest run --workspace` green including tool unit tests

## QA scenarios

- Happy: worker reports a noticed defect -> Triage issue with class + evidence links; evidence `.omo/evidence/<task-slug>/report-issue.txt`
- Failure: a report attempting to set Status=Ready or assign itself -> refused (typed); injection payload in description -> quoted data only; evidence `.omo/evidence/<task-slug>/report-refusals.txt`

## Non-goals

No auto-triage; no scheduling; no self-fix (report ≠ self-fix).

## Security impact

Untrusted-content boundary (same as E0-T9).

## Testing requirements

Refusal + injection negatives.

## Documentation impact

Tool reference docs; CHANGELOG `[Unreleased]`.

## Dependencies

None.

## Rollback implications

Filed issues closable; tool disable via config.
""",
    },
]
