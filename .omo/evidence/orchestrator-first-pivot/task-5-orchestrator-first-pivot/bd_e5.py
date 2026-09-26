#!/usr/bin/env python3
"""E5 leaf tasks: Arbitraitor worker mediation (all Risk=Critical).

E5-T7 and E5-T8 are gap-linked (Status=Backlog, `blocked:arbitraitor` label,
cross-repo blockedBy edges to the two arbsec/arbitraitor issues).
"""

TASKS = [
    {
        "id": "E5-T1",
        "epic": "E5",
        "title": "Task: mediation-sandbox — Restricted sandbox wiring + effective-controls preflight (full depth)",
        "risk": "Critical",
        "estimate": 8,
        "labels": ["needs-human-review"],
        "blocked_by": [],
        "body": """## Objective

Full-depth sandbox wiring for all workers: spawn under
`arbitraitor_sandbox::SandboxMode::Restricted` via `configure_command` /
`apply_sandbox`, preflight `compute_effective_controls()` per spawn with the
matrix recorded, fail-closed on any missing required control, and the
per-session capability report surfaced (§9.32.4 shape).

## References

- `docs/spec/40-arbitraitor-integration.md` §9.6 (sandbox integration)
- `docs/spec/40-arbitraitor-integration.md` §6.7 (fail closed)
- `docs/spec/tech-stack.md` §2.2 (`SandboxMode::Restricted`, `compute_effective_controls`, `configure_command`, `apply_sandbox`)

## Acceptance criteria

- [ ] Every worker spawn runs the preflight and records the controls matrix + verdict in run state
- [ ] Missing required control -> typed refusal naming the control; no worker process starts
- [ ] Per-session capability report available to `orc doctor` and `capability.check`
- [ ] `cargo nextest run --workspace` green; adversarial negatives assert forbidden effects did not occur

## QA scenarios

- Happy: reference Linux platform -> Required controls Available; worker runs; matrix recorded; evidence `.omo/evidence/<task-slug>/sandbox-preflight.txt`
- Failure: fixture-missing control -> refusal with control named; a sandbox-escape attempt fixture (write outside workspace) leaves no file (assert); evidence `.omo/evidence/<task-slug>/sandbox-fail-closed.txt`

## Non-goals

No sandbox implementation (Arbitraitor's); no macOS/Windows (ADR-0024); no Disposable mode.

## Security impact

The core containment wiring: Risk=Critical, needs-human-review.

## Testing requirements

Adversarial containment negatives (§21.4: assert the forbidden effect did not happen).

## Documentation impact

Mediation docs; CHANGELOG `[Unreleased]`.

## Dependencies

None upstream (deepens E0-T5).

## Rollback implications

Fail-closed; no non-secure bypass on this path.
""",
    },
    {
        "id": "E5-T2",
        "epic": "E5",
        "title": "Task: mediation-exec — `arbitraitor_exec::ExecutionContextBuilder` mediated bash (full policy surface)",
        "risk": "Critical",
        "estimate": 8,
        "labels": ["needs-human-review"],
        "blocked_by": ["E5-T1"],
        "body": """## Objective

Full mediated-exec wiring: every worker bash call goes through
`arbitraitor_exec::ExecutionContextBuilder` with explicit ExecutionPolicy,
NetworkPolicy, EnvAllowlist/EnvDenyList, and ResourceLimits; receipts
produced per execution; no direct process spawn anywhere on the worker path.

## References

- `docs/spec/40-arbitraitor-integration.md` §9.10 (command and script analysis)
- `docs/spec/40-arbitraitor-integration.md` §9.12 (network enforcement)
- `docs/spec/tech-stack.md` §2.2 (`ExecutionContextBuilder`, `NetworkPolicy`, `EnvAllowlist`, `EnvDenyList`, `ResourceLimits`)

## Acceptance criteria

- [ ] All worker bash is mediated with the full policy surface; a workspace-wide test asserts no `std::process::Command` on worker paths
- [ ] Network-denied exec leaves no connection (assert); env outside the allowlist is not visible to the child (assert)
- [ ] Resource limits enforced (fixture exceeding limits is killed; output truncated per policy)
- [ ] `cargo nextest run --workspace` green including mediation integration tests

## QA scenarios

- Happy: mediated bash with allowlisted env + network policy executes and produces a receipt; evidence `.omo/evidence/<task-slug>/exec-mediated.txt`
- Failure: denied network call -> no connection (assert via socket fixture); denied env var -> absent from child env; oversized output -> truncated + receipt notes it; evidence `.omo/evidence/<task-slug>/exec-denials.txt`

## Non-goals

No policy authoring (Arbitraitor's); no shell reimplementation.

## Security impact

Execution boundary: Risk=Critical, needs-human-review.

## Testing requirements

Adversarial negatives per policy dimension (network/env/resources).

## Documentation impact

Mediation docs; CHANGELOG `[Unreleased]`.

## Dependencies

- Blocked by E5-T1 (sandbox first).

## Rollback implications

Fail-closed path; no bypass.
""",
    },
    {
        "id": "E5-T3",
        "epic": "E5",
        "title": "Task: mediation-preflight-gate — capability preflight fail-closed gate (startup + per-spawn)",
        "risk": "Critical",
        "estimate": 5,
        "labels": ["needs-human-review"],
        "blocked_by": ["E5-T1"],
        "body": """## Objective

The capability preflight gate: at daemon/loop startup AND before every
worker spawn, probe Arbitraitor's capability report for the operation shape
about to run; any missing capability blocks the run (fail closed) and routes
to the gap workflow (E5-T6) — never a silent downgrade, never a local
workaround.

## References

- `docs/spec/40-arbitraitor-integration.md` §6.7 (sole security authority; fail closed)
- `docs/spec/40-arbitraitor-integration.md` §16.2 (mandatory security feature workflow)
- `docs/spec/10-orchestrator.md` §9.36 (kick-off conditions respect capability)

## Acceptance criteria

- [ ] Startup probe + per-spawn gate implemented; missing capability -> typed block + gap workflow trigger
- [ ] The gate's decision is recorded (probe result + verdict) per spawn
- [ ] `cargo nextest run --workspace` green; negative tests assert blocked spawns produced no worker process

## QA scenarios

- Happy: all capabilities present -> spawn proceeds; evidence `.omo/evidence/<task-slug>/gate-pass.txt`
- Failure: fixture-missing capability -> no worker process exists after the refused spawn (assert pid absence), gap issue path invoked; evidence `.omo/evidence/<task-slug>/gate-block.txt`

## Non-goals

No capability implementation; no "non-secure mode" on this path.

## Security impact

The fail-closed invariant itself: Risk=Critical, needs-human-review.

## Testing requirements

Assert-absence negatives (never claim pass merely because an error occurred).

## Documentation impact

Mediation docs; CHANGELOG `[Unreleased]`.

## Dependencies

- Blocked by E5-T1 (probe surface).

## Rollback implications

Fail-closed; nothing to roll back.
""",
    },
    {
        "id": "E5-T4",
        "epic": "E5",
        "title": "Task: mediation-lifecycle — lease/cancellation wiring onto the §9.24 lifecycle",
        "risk": "Critical",
        "estimate": 5,
        "labels": ["needs-human-review"],
        "blocked_by": ["E5-T1"],
        "body": """## Objective

Wire worker supervision onto the §9.24 lifecycle: leases with TTLs,
heartbeats local-only, lease expiry -> `orphaned` (never direct `failed`),
cancellation that releases resources promptly and visibly, checkpoint
resume for `approval-required` / `input-required` stable states.

## References

- `docs/spec/10-orchestrator.md` §9.24 (task and session lifecycle; §9.24.2 cancellation/recovery/leases/idempotency)
- `docs/spec/10-orchestrator.md` §9.27.4 (cancellation releases resources promptly and visibly)
- `crates/orchestraitor-lifecycle/` (existing state-machine scaffolding to drive)

## Acceptance criteria

- [ ] Lease acquire/expire/reap transitions validated by the existing lifecycle state machine; orphaned -> re-run fresh
- [ ] Cancellation kills the worker, reclaims the worktree/lease, and records the release (visible)
- [ ] `approval-required`/`input-required` hold the lease and resume from checkpoint
- [ ] `cargo nextest run --workspace` green including lifecycle integration tests (virtual clock)

## QA scenarios

- Happy: lease expiry -> orphaned -> fresh re-run; cancellation -> resources released (worktree gone, lease freed — assert); evidence `.omo/evidence/<task-slug>/lifecycle-transitions.txt`
- Failure: cancel during a mediated exec -> the child process is gone (assert pid absence) and the receipt records cancellation; evidence `.omo/evidence/<task-slug>/cancel-during-exec.txt`

## Non-goals

No reaper daemon (E8); no budget logic.

## Security impact

Cancellation correctness is containment-adjacent (prompt resource release): Risk=Critical, needs-human-review.

## Testing requirements

Virtual-clock transition tests; assert-absence cancellation negatives.

## Documentation impact

Lifecycle docs; CHANGELOG `[Unreleased]`.

## Dependencies

- Blocked by E5-T1 (mediated spawn first).

## Rollback implications

State-machine transitions; recoverable by construction.
""",
    },
    {
        "id": "E5-T5",
        "epic": "E5",
        "title": "Task: mediation-receipts-approvals — receipts + approval-required handling",
        "risk": "Critical",
        "estimate": 5,
        "labels": ["needs-human-review"],
        "blocked_by": ["E5-T3"],
        "body": """## Objective

Receipts and approvals for mediated work: every security-sensitive operation
produces an Arbitraitor receipt (retained, exportable); approval-demanding
operations transition the worker to the stable `approval-required` state
with checkpoint, surfaced to the trusted UI path — never approved by
agent-generated text (§6.4).

## References

- `docs/spec/40-arbitraitor-integration.md` §9.9 (approval integration — `PlanContext`, `ApprovalTokenIssuer`)
- `docs/spec/40-arbitraitor-integration.md` §9.14 (output quarantine and promotion)
- `docs/spec/40-arbitraitor-integration.md` §6.4 (approval belongs to the trusted UI)
- `docs/spec/tech-stack.md` §2.3 (mandatory MCP wiring — explicit `McpServer` construction)

## Acceptance criteria

- [ ] Every security-sensitive mediated op yields a retained receipt; receipt export works (`orc evidence export` shape)
- [ ] Approval-demanding op -> `approval-required` stable state + checkpoint; resume after approval
- [ ] Agent-generated approval text is never trusted (adversarial fixture)
- [ ] `cargo nextest run --workspace` green including approval-path integration tests

## QA scenarios

- Happy: approval-demanding fixture -> state + checkpoint; approval via trusted path -> resume; receipts complete; evidence `.omo/evidence/<task-slug>/approval-flow.txt`
- Failure: fake approval text in tool output -> not trusted (assert state unchanged); evidence `.omo/evidence/<task-slug>/fake-approval.txt`

## Non-goals

No headless prompt implementation (that is the Arbitraitor gap — E5-T7); no approval UI design beyond the trusted-path contract.

## Security impact

Approval boundary: Risk=Critical, needs-human-review.

## Testing requirements

Fake-approval adversarial negative; receipt-completeness assertions.

## Documentation impact

Mediation docs; CHANGELOG `[Unreleased]`.

## Dependencies

- Blocked by E5-T3 (gate first). Headless prompt UX additionally waits on E5-T7 (upstream gap).

## Rollback implications

Stable states; recoverable by design.
""",
    },
    {
        "id": "E5-T6",
        "epic": "E5",
        "title": "Task: mediation-gap-helper — `file_arbitraitor_gap` helper (missing capability → upstream issue + blocked wait)",
        "risk": "Critical",
        "estimate": 3,
        "labels": ["needs-human-review"],
        "blocked_by": [],
        "body": """## Objective

The `file_arbitraitor_gap` helper: when a required Arbitraitor capability is
missing, open a canonical issue in `arbsec/arbitraitor` with reproducible
context, link it from the blocked Orchestraitor task via a native cross-repo
blockedBy edge + the `blocked:arbitraitor` label, and hold a hard wait (never
retried as transient, §9.26.1) while bug-flow and other epic work continue.

## References

- `docs/spec/40-arbitraitor-integration.md` §16.2 (mandatory security feature workflow)
- `.agents/project/orchestraitor-workflow.md` (ownership boundaries; cross-repo blocker handling)
- `docs/spec/10-orchestrator.md` §9.40 (`blocked:arbitraitor` = hard wait)
- `docs/spec/10-orchestrator.md` §9.37(c) (routing follows the ownership table)

## Acceptance criteria

- [ ] A detected gap produces an arbsec/arbitraitor issue with reproducible context (capability, operation shape, probe result, affected Orchestraitor task)
- [ ] The Orchestraitor task gains the cross-repo blockedBy edge + `blocked:arbitraitor` label and leaves the ready queue
- [ ] The hard wait is never retried as transient (retry-class test); other work continues
- [ ] `cargo nextest run --workspace` green including helper unit tests (fixture API)

## QA scenarios

- Happy: fixture gap -> upstream issue + edge + label; queue excludes the task; other-epic work unaffected; evidence `.omo/evidence/<task-slug>/gap-filed.txt`
- Failure: a transient network error during filing is retried per §9.26.2 backoff, but a filed gap is NEVER retried as transient (assert retry classification); evidence `.omo/evidence/<task-slug>/gap-retry-classification.txt`

## Non-goals

No workaround implementation; no arbitraitor-repo changes beyond the filed issues.

## Security impact

Ownership-boundary enforcement: Risk=Critical, needs-human-review (a missed gap = an unmediated path).

## Testing requirements

Retry-classification negatives; queue-exclusion assertions.

## Documentation impact

Gap workflow docs; CHANGELOG `[Unreleased]`.

## Dependencies

None upstream.

## Rollback implications

Filed issues closable; edges removable.
""",
    },
    {
        "id": "E5-T7",
        "epic": "E5",
        "title": "Task: mediation-headless-approval — headless approval path for workers (BLOCKED on arbitraitor gap)",
        "risk": "Critical",
        "estimate": 5,
        "labels": ["needs-human-review", "blocked:arbitraitor"],
        "blocked_by": ["E5-T5", "GAP-HEADLESS-APPROVAL"],
        "status": "Backlog",
        "body": """## Objective

Consume a headless/non-interactive `arbitraitor_mcp::ApprovalPrompt`
implementation once Arbitraitor ships one: surface approval-required to the
orchestrator's trusted UI path (operator notification + explicit
resolution), persisting pending approvals as durable state. Today only
`StdinApprovalPrompt` exists, which cannot serve one-shot headless workers.

## References

- Upstream: arbsec/arbitraitor issue "Headless/non-interactive ApprovalPrompt implementation (arbitraitor_mcp::ApprovalPrompt has only StdinApprovalPrompt)" — this task's blocker (cross-repo blockedBy edge)
- `docs/spec/40-arbitraitor-integration.md` §9.9 (approval integration)
- `docs/spec/40-arbitraitor-integration.md` §6.4 (approval belongs to the trusted UI)
- Ledger F15 (only `StdinApprovalPrompt` shipped; ADR-0013 plan-bound approval model)

## Acceptance criteria

- [ ] Blocked until the upstream headless ApprovalPrompt lands; this task stays out of the ready queue (hard wait, `blocked:arbitraitor`)
- [ ] Once unblocked: headless workers surface approval-required via the trusted path; no stdin dependency; resume-from-checkpoint works
- [ ] `cargo nextest run --workspace` green including headless-approval integration tests

## QA scenarios

- Happy (post-unblock): approval-demanding fixture in a headless run -> pending approval persisted -> resolved via trusted path -> resume; evidence `.omo/evidence/<task-slug>/headless-approval.txt`
- Failure: approval denied -> task transitions to a typed blocked state (never auto-approved); evidence `.omo/evidence/<task-slug>/headless-denied.txt`

## Non-goals

No approval-prompt implementation in Orchestraitor (Arbitraitor owns it); no auto-approval.

## Security impact

Approval boundary: Risk=Critical, needs-human-review.

## Testing requirements

Post-unblock integration tests; denial negatives.

## Documentation impact

Mediation docs; CHANGELOG `[Unreleased]` when implemented.

## Dependencies

- Blocked by E5-T5 (approval handling) and the upstream arbitraitor gap issue (cross-repo blockedBy; hard wait per §9.26.1/§9.40).

## Rollback implications

Not started until unblocked (Status=Backlog).
""",
    },
    {
        "id": "E5-T8",
        "epic": "E5",
        "title": "Task: mediation-stable-embedding — consume the stable Arbitraitor embedding API (BLOCKED on ADR-0038)",
        "risk": "Critical",
        "estimate": 5,
        "labels": ["needs-human-review", "blocked:arbitraitor"],
        "blocked_by": ["GAP-STABLE-EMBEDDING"],
        "status": "Backlog",
        "body": """## Objective

Migrate Orchestraitor's Arbitraitor consumption onto the stable embedding
API surface once ADR-0038 (pipeline-engine crate extraction, currently
Proposed) lands: consume `arbitraitor-engine` + the engine-owned
`InspectionResultReceipt` wrapper instead of ad-hoc composition over the
individual crates, removing the current divergence risk (three compositions
with different coverage today).

## References

- Upstream: arbsec/arbitraitor issue "Land ADR-0038 pipeline-engine extraction (arbitraitor-engine) to give embedders one stable pipeline surface" — this task's blocker (cross-repo blockedBy edge)
- `docs/spec/tech-stack.md` §2.2 note (ADR-0038 staged extraction; until then, direct dependency on individual crates is the only option)
- Ledger F15 (ADR-0038 Proposed; 3 divergent pipeline compositions in CLI/daemon/MCP)

## Acceptance criteria

- [ ] Blocked until ADR-0038 extraction lands; hard wait, `blocked:arbitraitor`, out of the ready queue
- [ ] Once unblocked: Orchestraitor depends on `arbitraitor-engine` (or the ADR-0038-documented surface) with no ad-hoc pipeline composition; `cargo deny check` + `cargo audit` clean
- [ ] `cargo nextest run --workspace` green after migration

## QA scenarios

- Happy (post-unblock): workspace builds against the engine surface; receipts use the engine-owned wrapper; evidence `.omo/evidence/<task-slug>/engine-migration.txt`
- Failure: any residual ad-hoc composition -> CI grep/test fails the migration; evidence `.omo/evidence/<task-slug>/no-adhoc-composition.txt`

## Non-goals

No engine implementation (Arbitraitor's); no API design for them.

## Security impact

Integration-boundary migration: Risk=Critical, needs-human-review.

## Testing requirements

Post-unblock migration tests; composition-purity check.

## Documentation impact

tech-stack dependency notes update; CHANGELOG `[Unreleased]` when implemented.

## Dependencies

- Blocked by the upstream ADR-0038 landing (cross-repo blockedBy; hard wait).

## Rollback implications

Not started until unblocked (Status=Backlog).
""",
    },
]
