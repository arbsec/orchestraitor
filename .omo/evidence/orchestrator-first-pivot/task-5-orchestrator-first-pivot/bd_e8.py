#!/usr/bin/env python3
"""E8 leaf tasks: watch daemon + budgets + epic-focus controls.

Guard/budget-touching tasks (T2-T6) are Risk=Critical + needs-human-review.
"""

from bd_common import BUDGET_BLOCK

TASKS = [
    {
        "id": "E8-T1",
        "epic": "E8",
        "title": "Task: daemon-watch — `orcd watch` poll tick (adaptive cadence, reconcile per tick)",
        "risk": "High",
        "estimate": 5,
        "labels": [],
        "blocked_by": [],
        "body": """## Objective

The always-running watch daemon's poll tick: polls the board provider on a
fixed default cadence, operator-configurable through the §9.22 layers and
adapted to provider rate-limit feedback. Every tick is a reconcile pass:
board-wins sync with `board-diverged` events, promotion of newly unblocked
tasks, and re-evaluation of kick-off conditions (new eligible work, unblocked
task, released lease/worker slot, reset budget window). The daemon owns no
decisions — campaign sessions decide; the daemon executes and supervises.

## References

- `docs/spec/10-orchestrator.md` §9.36 (watch daemon — poll tick, kick-off conditions)
- `docs/spec/10-orchestrator.md` §9.43 (reconcile semantics per tick)
- Ledger D5 (polling-first; default 30s tick, rate-limit-adaptive)

## Acceptance criteria

- [ ] `orcd watch` runs the tick loop; cadence adapts to rate-limit feedback (virtual-clock test)
- [ ] Each tick performs the reconcile pass (board-wins + events) and re-evaluates kick-off conditions
- [ ] The daemon never nudges a human and never bypasses a budget to keep the loop moving
- [ ] `cargo nextest run --workspace` green including tick unit tests

## QA scenarios

- Happy: seeded board change -> next tick reconciles + kicks a campaign session; evidence `.omo/evidence/<task-slug>/watch-tick.txt`
- Failure: rate-limit feedback -> cadence backs off (assert delay growth); a tick that errors is logged and the loop continues (no crash-loop); evidence `.omo/evidence/<task-slug>/watch-backoff.txt`

## Non-goals

No stall reaping (E8-T2); no budgets (E8-T3/T4); no controls (E8-T5/T6); no webhook receiver.

## Security impact

Daemon core (unprivileged; foreground + systemd-user documented modes).

## Testing requirements

Virtual-clock tick tests; backoff tests.

## Documentation impact

`docs/cli/` daemon docs; CHANGELOG `[Unreleased]`.

## Dependencies

None upstream (consumes E1 provider + E7 sessions at runtime).

## Rollback implications

Stopping the daemon leaves board state intact; board-wins on restart.
""",
    },
    {
        "id": "E8-T2",
        "epic": "E8",
        "title": "Task: daemon-stall-reaper — stall reaping + orphan detection (leases, TTLs, reaper interval)",
        "risk": "Critical",
        "estimate": 5,
        "labels": ["needs-human-review"],
        "blocked_by": ["E8-T1"],
        "body": """## Objective

Stall and orphan enforcement per §9.36/§9.24: heartbeats are local, lease
expiry transitions to `orphaned` (never direct `failed`), the reaper walks
running tasks on its configured interval, a campaign session producing no
decision record within its lease is orphaned and re-run fresh, and a stalled
worker is detected the same way and killed.

## References

- `docs/spec/10-orchestrator.md` §9.36 (stall and orphan detection)
- `docs/spec/10-orchestrator.md` §9.24 (leases + TTLs; reaper interval; §9.24.2 recovery)
- Ledger F3 (spec §9.24 already defines the primitives — this task drives them)

## Acceptance criteria

- [ ] Lease expiry -> `orphaned` transition (never `failed`); reaper walks running tasks on interval
- [ ] A stalled worker is killed; the task transitions to a typed stalled state; the kill is recorded
- [ ] A campaign session with no decision within its lease is orphaned and re-run fresh
- [ ] `cargo nextest run --workspace` green; reaper tests use a virtual clock; kill assertions verify the process is gone

## QA scenarios

- Happy: fixture stalled worker (no heartbeat past the stall timeout) -> reaped; evidence `.omo/evidence/<task-slug>/reap-stalled.txt`
- Failure: a healthy worker (heartbeats flowing) is NOT reaped (assert); a reaped worker's process is verifiably gone (assert pid absence); evidence `.omo/evidence/<task-slug>/reap-precision.txt`

## Non-goals

No budgets; no controls; no cross-machine supervision.

## Security impact

Guard-touching (kills processes): Risk=Critical, needs-human-review — a too-aggressive or too-lax reaper is a safety defect.

## Testing requirements

Virtual-clock reaper tests; false-reap negatives; assert-absence kill checks.

## Documentation impact

Daemon docs; CHANGELOG `[Unreleased]`.

## Dependencies

- Blocked by E8-T1 (tick loop).

## Rollback implications

Reaper interval configurable; transitions recoverable by design.
""",
    },
    {
        "id": "E8-T3",
        "epic": "E8",
        "title": "Task: daemon-budgets-run-spend — run + spend budget enforcement ($10/day soft cap, 4h run budget)",
        "risk": "Critical",
        "estimate": 5,
        "labels": ["needs-human-review"],
        "blocked_by": ["E8-T1"],
        "body": """## Objective

Run/time and monetary-spend budget enforcement before any spawn: per-task and
per-campaign run budgets, calendar-durable daily spend caps, and the
failure behavior — a budget stop produces an explicit `blocked` or
`needs-human` state, never a silent skip and never a weakened retry.

## References

- `docs/spec/10-orchestrator.md` §9.36 (budget enforcement — classes + failure behavior)
- `docs/spec/30-model-routing.md` §9.19.6 (budget scopes)
- Ledger D8 (budget VALUES live in task bodies, owner-adjustable at the board)

## Acceptance criteria

- [ ] Before any spawn the daemon enforces run/time budgets (per task, per campaign) and monetary spend (daily, calendar-durable)
- [ ] Default values (owner-adjustable at the board): """ + BUDGET_BLOCK + """
- [ ] A budget stop produces blocked/needs-human naming the limiting budget; never a silent skip
- [ ] `cargo nextest run --workspace` green; budget tests use a virtual clock + fixture ledger

## QA scenarios

- Happy: spend under cap -> spawn proceeds; run under budget -> proceeds; evidence `.omo/evidence/<task-slug>/budgets-pass.txt`
- Failure: spend at cap -> no spawn + needs-human naming spend; run over 4h -> stop + blocked; calendar rollover resets the daily cap (virtual-clock test); evidence `.omo/evidence/<task-slug>/budgets-stop.txt`

## Non-goals

No subscription budgets (E8-T4); no per-tool budgets (E4-T3); no payment integration.

## Security impact

Guard-touching (spend safety): Risk=Critical, needs-human-review.

## Testing requirements

Cap/rollover tests; never-silent-skip negatives; guardrail-weakening fixture (config attempting to disable budgets is rejected or visibly labelled).

## Documentation impact

Daemon docs + budget config reference; CHANGELOG `[Unreleased]`.

## Dependencies

- Blocked by E8-T1 (tick loop).

## Rollback implications

Budgets configurable; stops are visible states.
""",
    },
    {
        "id": "E8-T4",
        "epic": "E8",
        "title": "Task: daemon-budgets-subscription — subscription-usage budget class (wired to §9.46)",
        "risk": "Critical",
        "estimate": 3,
        "labels": ["needs-human-review"],
        "blocked_by": ["E8-T3"],
        "body": """## Objective

The subscription-usage budget class in the daemon's pre-spawn enforcement:
subscription exhaustion (per §9.46's all-exhausted stop) is a budget stop
like run and spend — the daemon stops spawning the affected role's work and
emits needs-human, never silently falling through to metered spend.

## References

- `docs/spec/10-orchestrator.md` §9.36 (budget classes incl. subscription)
- `docs/spec/30-model-routing.md` §9.46 (subscription-aware routing — the router side)
- Ledger D6 (subscription spend guards)

## Acceptance criteria

- [ ] Pre-spawn enforcement includes the subscription class; all-exhausted (from the router's eligibility gate) stops the role's work + needs-human
- [ ] The stop names the exhausted subscriptions; no metered fall-through unless explicitly configured
- [ ] `cargo nextest run --workspace` green including subscription-budget unit tests

## QA scenarios

- Happy: subscription with remaining usage -> spawn proceeds; evidence `.omo/evidence/<task-slug>/sub-budget-pass.txt`
- Failure: all subscriptions exhausted -> no spawn + needs-human naming them; explicit-override config -> fall-through only when configured and recorded; evidence `.omo/evidence/<task-slug>/sub-budget-stop.txt`

## Non-goals

No usage introspection APIs (E2-T6 owns state); no provider integrations.

## Security impact

Guard-touching (spend safety): Risk=Critical, needs-human-review.

## Testing requirements

Exhaustion + override negatives.

## Documentation impact

Daemon docs; CHANGELOG `[Unreleased]`.

## Dependencies

- Blocked by E8-T3 (budget framework).

## Rollback implications

Configurable; stops are visible.
""",
    },
    {
        "id": "E8-T5",
        "epic": "E8",
        "title": "Task: daemon-backlog-controls — `orc backlog pause/resume/cancel/retry/skip/reprioritize/assign`",
        "risk": "Critical",
        "estimate": 5,
        "labels": ["needs-human-review"],
        "blocked_by": ["E8-T1"],
        "body": """## Objective

The operator's backlog controls per §9.33.6: `orc backlog pause|resume|
cancel|retry|skip|assign|status|show` — durable, board-visible control
surface over the loop. Pause lets in-flight work finish per lease semantics
while new spawns are blocked; cancel/retry/skip/reprioritize/assign map to
guarded board transitions + run-state updates.

## References

- `docs/spec/10-orchestrator.md` §9.33.6 (durability and control — the verbatim control set)
- `docs/spec/10-orchestrator.md` §9.24 (lease semantics on pause)
- `docs/spec/10-orchestrator.md` §9.39 (board.move — the guarded path controls ride)

## Acceptance criteria

- [ ] All controls implemented; each is durable (survives restart) and board-visible
- [ ] Pause: in-flight work finishes per lease; new spawns blocked; resume lifts it
- [ ] Controls validate against the same guards as board.move (no policy-violating transitions)
- [ ] `cargo nextest run --workspace` green including control unit tests

## QA scenarios

- Happy: pause -> running task completes, no new spawns; resume -> spawns resume; retry re-queues a typed-failed task; evidence `.omo/evidence/<task-slug>/backlog-controls.txt`
- Failure: cancel on a task another session holds -> lease-checked refusal; a control attempting a policy-invalid transition -> typed refusal; evidence `.omo/evidence/<task-slug>/backlog-refusals.txt`

## Non-goals

No epic-focus controls (E8-T6); no chat steering (E9-T3 maps onto these).

## Security impact

Guard-touching (pause/cancel are loop-safety controls): Risk=Critical, needs-human-review.

## Testing requirements

Durability (restart) tests; lease/refusal negatives.

## Documentation impact

`docs/cli/` backlog reference; CHANGELOG `[Unreleased]`.

## Dependencies

- Blocked by E8-T1 (daemon surface).

## Rollback implications

Controls are guarded transitions — revertible on the board.
""",
    },
    {
        "id": "E8-T6",
        "epic": "E8",
        "title": "Task: daemon-epic-focus — `orc epic focus/pause/resume` + `auto_advance_epic` opt-in",
        "risk": "Critical",
        "estimate": 3,
        "labels": ["needs-human-review"],
        "blocked_by": ["E8-T1"],
        "body": """## Objective

Epic-focus controls per §9.41: `orc epic focus <epic>` switches the active
epic; `orc epic pause` pauses focus (in-flight work finishes per lease
semantics while new spawns are blocked); `orc epic resume` lifts the pause.
Exhausted or wholly-blocked active epic -> needs-human signal while
bug-flow continues; the loop NEVER silently auto-switches unless
`auto_advance_epic` is explicitly configured (default: visible stop).

## References

- `docs/spec/10-orchestrator.md` §9.41 (epic-focus scheduling policy — controls, exhaustion, auto_advance_epic)
- `docs/spec/10-orchestrator.md` §9.40 (wholly-blocked epic -> needs-human)

## Acceptance criteria

- [ ] focus/pause/resume implemented with lease-safe semantics; focus state is durable
- [ ] Exhausted/wholly-blocked epic -> needs-human + bug-flow continues (test)
- [ ] No silent auto-switch by default; `auto_advance_epic` opt-in recorded when configured
- [ ] `cargo nextest run --workspace` green including focus-control unit tests

## QA scenarios

- Happy: focus E0 -> queue promotes only E0 tasks; pause -> in-flight finishes, no new spawns; evidence `.omo/evidence/<task-slug>/epic-focus.txt`
- Failure: exhausted epic without auto_advance -> visible stop (needs-human), no switch; with auto_advance -> switch happens and is recorded; evidence `.omo/evidence/<task-slug>/epic-exhaustion.txt`

## Non-goals

No chat steering (E9-T3); no multi-epic focus (config keeps max_active_epics).

## Security impact

Guard-touching (focus switching changes what work runs): Risk=Critical, needs-human-review.

## Testing requirements

Lease-safety + no-silent-switch negatives.

## Documentation impact

`docs/cli/` epic reference; CHANGELOG `[Unreleased]`.

## Dependencies

- Blocked by E8-T1 (daemon surface).

## Rollback implications

Focus state is config/board data; revertible.
""",
    },
    {
        "id": "E8-T7",
        "epic": "E8",
        "title": "Task: daemon-restart-recovery — crash-safe restart from durable state",
        "risk": "High",
        "estimate": 3,
        "labels": [],
        "blocked_by": ["E8-T1"],
        "body": """## Objective

Restart recovery per §9.36/§9.24.2: on restart the daemon resumes from
durable state — `paused` stays paused, `running` becomes `orphaned`, the
poll tick resumes, and no in-flight decision is lost (campaign sessions are
safe by construction; workers recover per lease semantics).

## References

- `docs/spec/10-orchestrator.md` §9.36 (the daemon is itself crash-safe)
- `docs/spec/10-orchestrator.md` §9.24.2 (cancellation, recovery, leases, idempotency)
- Ledger D5 (durable state homes: board + local sqlite run state)

## Acceptance criteria

- [ ] Kill -9 the daemon -> restart resumes: paused stays paused, running -> orphaned -> re-run, tick resumes
- [ ] No duplicated side effects after recovery (idempotency receipts asserted)
- [ ] `cargo nextest run --workspace` green including restart-recovery integration tests

## QA scenarios

- Happy: kill + restart -> state machine correct, loop continues; evidence `.omo/evidence/<task-slug>/restart-recovery.txt`
- Failure: corrupted local run-state -> typed recovery error with guidance (no panic, no silent state loss); evidence `.omo/evidence/<task-slug>/restart-corrupt.txt`

## Non-goals

No multi-machine failover; no board-state repair (E1-T5 owns reconcile).

## Security impact

Recovery correctness (no authority resurrection beyond lease semantics).

## Testing requirements

Kill/restart integration tests; idempotency assertions.

## Documentation impact

Daemon docs; CHANGELOG `[Unreleased]`.

## Dependencies

- Blocked by E8-T1 (tick loop).

## Rollback implications

Recovery is the rollback path.
""",
    },
]
