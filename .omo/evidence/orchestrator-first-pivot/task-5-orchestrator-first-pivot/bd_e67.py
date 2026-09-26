#!/usr/bin/env python3
"""E6 + E7 leaf tasks: delivery lifecycle + campaign session runner."""

TASKS = [
    {
        "id": "E6-T1",
        "epic": "E6",
        "title": "Task: lifecycle-worktree — worktree-per-task provisioning (trusted controller owns Git metadata)",
        "risk": "Medium",
        "estimate": 5,
        "labels": [],
        "blocked_by": [],
        "body": """## Objective

Full worktree-per-task provisioning: allocate, name (`type/<slug>`), track,
and reap worktrees; the trusted controller owns all Git metadata — the
worker never sees the main checkout's `.git`; stale-workspace cleanup on
task completion/failure.

## References

- `docs/spec/40-arbitraitor-integration.md` §6.2 (a worktree is not a sandbox)
- `docs/spec/20-harness-worker.md` §9.4 (workspace and Git controller)
- AGENTS.md (worktree-first rule)

## Acceptance criteria

- [ ] Worktrees provision/reap per task lifecycle; run state tracks the mapping (task -> worktree path)
- [ ] Worker sandbox cannot reach the main checkout (adversarial fixture: assert no write outside the worktree)
- [ ] Stale-workspace cleanup runs on terminal states; orphaned worktrees surfaced
- [ ] `cargo nextest run --workspace` green including provisioning unit tests

## QA scenarios

- Happy: provision -> run -> reap; mapping recorded; evidence `.omo/evidence/<task-slug>/worktree-lifecycle.txt`
- Failure: escape fixture (write to ../ from inside the worker) -> no file outside the worktree (assert); evidence `.omo/evidence/<task-slug>/worktree-containment.txt`

## Non-goals

No VFS/overlay backends (Arbitraitor-owned, §9.4.2); no multi-repo.

## Security impact

Git-metadata ownership is a trust boundary (§6.2).

## Testing requirements

Containment negatives; cleanup tests.

## Documentation impact

Workspace docs; CHANGELOG `[Unreleased]`.

## Dependencies

None upstream (deepens E0-T6).

## Rollback implications

Worktrees disposable; cleanup idempotent.
""",
    },
    {
        "id": "E6-T2",
        "epic": "E6",
        "title": "Task: lifecycle-dco — DCO commit path with bot attribution",
        "risk": "Low",
        "estimate": 3,
        "labels": [],
        "blocked_by": ["E6-T1"],
        "body": """## Objective

The commit path: conventional-commit messages, `Signed-off-by` DCO trailer,
bot attribution (git user.name/email convention for the App bot identity
per D11), and commit signing per repo rules where applicable.

## References

- AGENTS.md (Conventional Commits; DCO)
- Ledger D11 (bot attribution convention)
- `docs/spec/10-orchestrator.md` §9.33.2 (task traceability into the PR)

## Acceptance criteria

- [ ] Worker commits carry type-prefixed conventional messages + DCO trailer + bot identity
- [ ] Repo DCO ruleset accepts the commits (CI check green on fixture PRs)
- [ ] `cargo nextest run --workspace` green including commit-path unit tests

## QA scenarios

- Happy: fixture commit passes `git log --format='%B'` assertions + DCO check; evidence `.omo/evidence/<task-slug>/dco-commit.txt`
- Failure: missing sign-off -> the commit is rejected by the repo ruleset (assert via fixture push); evidence `.omo/evidence/<task-slug>/dco-rejected.txt`

## Non-goals

No merge commits (squash-merge policy); no signing-key management (keyring path is E0-T1).

## Security impact

Attribution integrity (auditable authorship).

## Testing requirements

Format assertions; ruleset-rejection negative.

## Documentation impact

Delivery docs; CHANGELOG `[Unreleased]`.

## Dependencies

- Blocked by E6-T1 (worktree first).

## Rollback implications

Commits are revertible; branches deletable.
""",
    },
    {
        "id": "E6-T3",
        "epic": "E6",
        "title": "Task: lifecycle-push-credential — scoped lease push credential (short-lived, repo-scoped)",
        "risk": "Critical",
        "estimate": 5,
        "labels": ["needs-human-review"],
        "blocked_by": ["E0-T1"],
        "body": """## Objective

The push credential path: mint a short-lived, repository-scoped credential
via the GitHub App installation (E0-T1) for each push lease; credentials
never persist beyond the lease; no ambient owner auth on the push path;
expiry mid-lease fails closed with a typed error and a retryable lease
refresh.

## References

- `docs/spec/10-orchestrator.md` §9.25.2 (short-lived, resource-scoped authority)
- `docs/spec/40-arbitraitor-integration.md` §9.23 (secret resolution; in-memory handling, zeroize)
- Ledger D11 (installation-token minting)

## Acceptance criteria

- [ ] Pushes authenticate with a lease-scoped token minted per delivery; token lifetime bounded by the lease
- [ ] No credential material in logs, error messages, or run state (§9.23.4 redaction; test)
- [ ] Expiry mid-push -> typed failure + lease refresh + retry (bounded per §9.26)
- [ ] `cargo nextest run --workspace` green including credential-path unit tests (fixture minter)

## QA scenarios

- Happy: push with minted lease token succeeds; token discarded after; evidence `.omo/evidence/<task-slug>/push-lease.txt`
- Failure: expired token -> typed failure, no fallback to ambient auth, refresh + bounded retry; redaction test greps logs for token material (zero hits); evidence `.omo/evidence/<task-slug>/push-expiry-redaction.txt`

## Non-goals

No long-lived tokens; no cross-repo credential reuse.

## Security impact

Credential handling: Risk=Critical, needs-human-review.

## Testing requirements

Redaction negatives; expiry/refresh tests.

## Documentation impact

Delivery docs; CHANGELOG `[Unreleased]`.

## Dependencies

- Blocked by E0-T1 (App identity + minting path).

## Rollback implications

Lease tokens expire naturally; path disableable.
""",
    },
    {
        "id": "E6-T4",
        "epic": "E6",
        "title": "Task: lifecycle-draft-pr — draft PR creation with spec anchors + evidence links",
        "risk": "Medium",
        "estimate": 3,
        "labels": [],
        "blocked_by": ["E6-T1", "E6-T2"],
        "body": """## Objective

Draft PR creation: PR body carries the task's spec anchors, acceptance
criteria status, QA evidence paths, and the checklist markers the
PR-lifecycle policy consumes; PR opens as draft; the board item links the PR.

## References

- `docs/spec/10-orchestrator.md` §9.33.2 (task fields; spec traceability)
- `.agents/project/orchestraitor-workflow.md` (PR convergence requirements the body must support)
- `.github/PULL_REQUEST_TEMPLATE.md` (checklist markers)

## Acceptance criteria

- [ ] PR body includes spec anchors (document-qualified `docs/spec/...` refs), evidence paths, and the repo's PR checklist markers
- [ ] PR opens as draft; board item links it (linked PR visible on the board)
- [ ] `cargo nextest run --workspace` green including PR-body unit tests

## QA scenarios

- Happy: fixture delivery -> draft PR with complete body; board link present; evidence `.omo/evidence/<task-slug>/draft-pr.txt`
- Failure: body missing required sections -> typed validation failure before opening (no incomplete PR); evidence `.omo/evidence/<task-slug>/pr-body-validation.txt`

## Non-goals

No merge behavior; no review automation (E0-T11/E7-T5 own triggers).

## Security impact

PR bodies are untrusted content for downstream agents (marker-wrapped evidence).

## Testing requirements

Body-validation tests; link assertions.

## Documentation impact

Delivery docs; CHANGELOG `[Unreleased]`.

## Dependencies

- Blocked by E6-T1, E6-T2 (worktree + commit path).

## Rollback implications

Draft PRs closable; links removable.
""",
    },
    {
        "id": "E6-T5",
        "epic": "E6",
        "title": "Task: lifecycle-risk-relabel — PR-time actual-diff Risk re-label (Critical + needs-human-review on security-touching diffs)",
        "risk": "Critical",
        "estimate": 3,
        "labels": ["needs-human-review"],
        "blocked_by": ["E6-T4"],
        "body": """## Objective

At PR time, re-derive the task's Risk from the ACTUAL diff (not the planned
one): when the diff touches security-sensitive areas (the Arbitraitor
integration boundary, provider transports/proxy, capability issuance, output
promotion, network/secret handling, `unsafe`), the item is re-labelled
Risk=Critical with the `needs-human-review` label and routed per the
workflow policy — even if the task was originally estimated lower. This is
the guardrail-weakening detector at the delivery gate.

## References

- `.agents/project/orchestraitor-workflow.md` (security-first review; required reviewer domains by touched area)
- `docs/spec/50-contracts-data.md` §21.1 (human review for security-sensitive changes)
- `docs/spec/10-orchestrator.md` §9.33.4 (review loop consumes the risk)

## Acceptance criteria

- [ ] A diff touching any security-sensitive area (fixture list from the workflow policy table) triggers Risk=Critical + `needs-human-review` + the security reviewer domain
- [ ] A diff NOT touching those areas leaves the risk unchanged (no false positives beyond the policy table)
- [ ] The re-label decision is recorded (areas matched) and visible on the board item
- [ ] `cargo nextest run --workspace` green including re-label unit tests over diff fixtures

## QA scenarios

- Happy: fixture diff touching `crates/orchestraitor-arb-client/` -> re-label to Critical + needs-human-review; evidence `.omo/evidence/<task-slug>/relabel-critical.txt`
- Failure: guardrail-weakening fixture (a diff that removes a mediation check while the task was Risk=Low) -> re-label fires (assert); a pure-docs diff -> no re-label; evidence `.omo/evidence/<task-slug>/relabel-guardrail.txt`

## Non-goals

No review automation; no merge authority; no risk DE-escalation (only escalation is automatic).

## Security impact

This IS a delivery-gate guard: Risk=Critical, needs-human-review.

## Testing requirements

Guardrail-weakening adversarial fixtures; false-positive negatives.

## Documentation impact

Delivery docs; CHANGELOG `[Unreleased]`.

## Dependencies

- Blocked by E6-T4 (draft PR exists first).

## Rollback implications

Labels are board fields — revertible; the record is append-only.
""",
    },
    # ---------------- E7 ----------------
    {
        "id": "E7-T1",
        "epic": "E7",
        "title": "Task: campaign-session — session-per-decision campaign session (full semantics)",
        "risk": "Medium",
        "estimate": 5,
        "labels": [],
        "blocked_by": [],
        "body": """## Objective

Full campaign semantics per §9.35: a fresh, bounded manager invocation that
reads the reconciled board state, applies the epic-focus scheduling policy,
selects at most one eligible unit of work, persists exactly ONE decision
record, and exits. A spawn decision hands the record to the watch daemon
(E8) — the campaign session never spawns workers itself. No long-lived
orchestrator conversation; no accumulated context; no decision outside a
recorded session.

## References

- `docs/spec/10-orchestrator.md` §9.35 (campaign orchestration: session-per-decision)
- `docs/spec/10-orchestrator.md` §9.33.3 (fresh contexts)
- Ledger D5 (manager session = one board pass -> one decision -> exit)

## Acceptance criteria

- [ ] One session = one decision record (spawn or no-op) then exit; no cross-session state
- [ ] Spawn decisions carry kind, selected task, role, model+provider, worker args, rationale, alternatives
- [ ] A crashed session is safe by construction (next tick runs a fresh one; no partial-decision recovery)
- [ ] `cargo nextest run --workspace` green; hermetic campaign tests (sqlite board + simulator)

## QA scenarios

- Happy: seeded board -> session selects per epic-focus + priority, records the decision, exits; evidence `.omo/evidence/<task-slug>/campaign-session.txt`
- Failure: kill mid-session -> no partial decision persisted (assert record count); next session runs fresh; evidence `.omo/evidence/<task-slug>/campaign-crash.txt`

## Non-goals

No daemon (E8); no worker supervision; no review-worker (E7-T5).

## Security impact

Manager output is structured, typed, recorded; capability requests explicit.

## Testing requirements

Hermetic integration; crash-safety assertions; determinism.

## Documentation impact

Campaign docs; CHANGELOG `[Unreleased]`.

## Dependencies

None upstream (deepens E0-T7; E8 daemon consumes the records).

## Rollback implications

Append-only decision records; sessions are disposable.
""",
    },
    {
        "id": "E7-T2",
        "epic": "E7",
        "title": "Task: campaign-focusstate — FocusState context assembly",
        "risk": "Medium",
        "estimate": 3,
        "labels": [],
        "blocked_by": ["E7-T1"],
        "body": """## Objective

FocusState context assembly: the campaign session's input context is the
reconciled board state filtered through the epic-focus policy — active
epic, its cross-board sub-graph, ready-queue contents with priorities,
blocked graph, budget consumption, and the no-op history — assembled fresh
per session from durable state, never from a previous session's memory.

## References

- `docs/spec/10-orchestrator.md` §9.35 (campaign reads reconciled state)
- `docs/spec/10-orchestrator.md` §9.41 (epic-focus policy — what the context carries)
- `docs/spec/10-orchestrator.md` §9.43 (durable homes the context reads)

## Acceptance criteria

- [ ] FocusState carries active epic, sub-graph, queue, blocked graph, budgets, no-op history — all stamped with last-synced
- [ ] Context assembly is deterministic from durable state (replay test)
- [ ] `cargo nextest run --workspace` green including assembly unit tests

## QA scenarios

- Happy: seeded board + budgets -> FocusState snapshot matches expected fixture; evidence `.omo/evidence/<task-slug>/focusstate.txt`
- Failure: stale board read -> last-synced stamp present and older than write time (assert visible staleness, never silent freshness); evidence `.omo/evidence/<task-slug>/focusstate-stale.txt`

## Non-goals

No context compiler integration (later); no chat surface.

## Security impact

Context is untrusted-content-bearing (board bodies) — marker-wrapped when quoted.

## Testing requirements

Determinism + staleness tests.

## Documentation impact

Campaign docs; CHANGELOG `[Unreleased]`.

## Dependencies

- Blocked by E7-T1 (session shell).

## Rollback implications

Read-only assembly; no state.
""",
    },
    {
        "id": "E7-T3",
        "epic": "E7",
        "title": "Task: campaign-noop-reasons — typed no-op reasons (empty-queue / all-blocked / epic-exhausted)",
        "risk": "Low",
        "estimate": 2,
        "labels": [],
        "blocked_by": ["E7-T1"],
        "body": """## Objective

Typed no-op reasons per §9.35: `empty-queue` (no eligible work exists at
all), `all-blocked` (eligible work exists but every candidate is blocked;
the blocked graph is attached), `epic-exhausted` (the active epic has no
remaining schedulable work). Distinct reasons are distinct states — never
a generic "nothing to do".

## References

- `docs/spec/10-orchestrator.md` §9.35 (no-op reasons)
- `docs/spec/10-orchestrator.md` §9.40 (blocked graph attachment)
- `docs/spec/10-orchestrator.md` §9.41 (epic-exhausted -> needs-human)

## Acceptance criteria

- [ ] Each of the three conditions produces its typed reason; all-blocked attaches the blocked graph
- [ ] No-op decisions are persisted like any decision record (kind=no-op + reason)
- [ ] `cargo nextest run --workspace` green including reason-classification unit tests

## QA scenarios

- Happy: three seeded boards (empty / all-blocked / exhausted epic) -> three distinct typed no-ops; evidence `.omo/evidence/<task-slug>/noop-reasons.txt`
- Failure: a board with one blocked + one eligible task -> NOT all-blocked (the eligible one is selected); misclassification fixture fails the test; evidence `.omo/evidence/<task-slug>/noop-misclassification.txt`

## Non-goals

No needs-human emission (E8 owns the signal surface); no chat exposure.

## Security impact

None (classification).

## Testing requirements

Classification tests incl. boundary cases.

## Documentation impact

Campaign docs; CHANGELOG `[Unreleased]`.

## Dependencies

- Blocked by E7-T1.

## Rollback implications

Append-only records.
""",
    },
    {
        "id": "E7-T4",
        "epic": "E7",
        "title": "Task: campaign-stop-conditions — typed stop conditions (budget stops → blocked/needs-human, never silent)",
        "risk": "Critical",
        "estimate": 3,
        "labels": ["needs-human-review"],
        "blocked_by": ["E7-T1"],
        "body": """## Objective

Typed stop conditions for campaign sessions and their workers: exhausting a
budget (attempts, re-plan, run/time, spend, subscription) produces an
explicit `blocked` or `needs-human` state with the limiting budget named —
never a silent skip, never a weakened retry, never a success-by-exhaustion.
Non-retriable classes (merge conflict, verification failure, policy denial)
never loop.

## References

- `docs/spec/10-orchestrator.md` §9.35 (session semantics)
- `docs/spec/10-orchestrator.md` §9.36 (budget enforcement — the daemon side)
- `docs/spec/10-orchestrator.md` §9.33.5 (failure classes)
- `docs/spec/10-orchestrator.md` §9.26 (retry semantics)

## Acceptance criteria

- [ ] Each budget class has a typed stop condition naming the budget and remaining state
- [ ] A stop produces blocked/needs-human on the board item + the decision record; no silent skip
- [ ] Non-retriable classes terminate without retry (test)
- [ ] `cargo nextest run --workspace` green including stop-condition unit tests (virtual clock)

## QA scenarios

- Happy: fixture exhausting attempts -> blocked + named budget + linked Bug (E0-T9 path); evidence `.omo/evidence/<task-slug>/stop-attempts.txt`
- Failure: guardrail-weakening fixture (a stop condition that would convert exhaustion into success) -> the suite catches it; evidence `.omo/evidence/<task-slug>/stop-guardrail.txt`

## Non-goals

No budget enforcement in the session (E8 daemon enforces; the session types and records).

## Security impact

Guard-touching (stop semantics are the anti-forever-loop guard): Risk=Critical, needs-human-review.

## Testing requirements

Guardrail-weakening adversarial fixtures; non-retriable negatives.

## Documentation impact

Campaign docs; CHANGELOG `[Unreleased]`.

## Dependencies

- Blocked by E7-T1.

## Rollback implications

Typed states; recoverable by design.
""",
    },
    {
        "id": "E7-T5",
        "epic": "E7",
        "title": "Task: campaign-review-worker — autonomous review-worker driving the existing adversarial-review loop (IS-6 leg)",
        "risk": "Medium",
        "estimate": 5,
        "labels": [],
        "blocked_by": ["E7-T1"],
        "body": """## Objective

The autonomous review-worker (IS-6 leg): a fresh-context reviewer session
that drives the EXISTING adversarial-review workflow to convergence —
requests reviews per the workflow policy, tracks threads/findings, reruns
review against the current HEAD on new commits, and surfaces convergence or
needs-human. The review-worker never merges, never approves its own work,
and never weakens review invariants; merge remains human-gated.

## References

- `docs/spec/10-orchestrator.md` §9.33.4 (change-set and PR review loop)
- `docs/spec/10-orchestrator.md` §9.33.3 (fresh-context reviewers)
- `.agents/project/orchestraitor-workflow.md` (PR convergence requirements — the policy this worker drives)
- AGENTS.md (review and merge invariants — the authority)

## Acceptance criteria

- [ ] The review-worker triggers review, deduplicates findings, tracks resolution, and reruns on new commits (targeting current HEAD)
- [ ] Reaching max_review_loops produces blocked/needs-human — never silent approval (hard ceiling respected)
- [ ] The worker never merges, never approves, never dismisses reviews (assert)
- [ ] `cargo nextest run --workspace` green including review-worker unit tests (fixture PRs)

## QA scenarios

- Happy: fixture PR with actionable findings -> worker drives resolution -> convergence detected -> human gate remains; evidence `.omo/evidence/<task-slug>/review-worker.txt`
- Failure: loop-limit fixture -> needs-human (not approval); a PR with unresolved blocking threads is never marked converged; evidence `.omo/evidence/<task-slug>/review-limits.txt`

## Non-goals

No new review policy (rides the existing one); no merge authority; no reviewer-model selection changes.

## Security impact

Preserves the adversarial-review invariant; merge-safety asserted in E10-T4.

## Testing requirements

Convergence + limit fixtures; never-merge assertions.

## Documentation impact

Campaign docs; CHANGELOG `[Unreleased]`.

## Dependencies

- Blocked by E7-T1 (session shell).

## Rollback implications

Worker is a driver over existing policy; disable via config.
""",
    },
]
