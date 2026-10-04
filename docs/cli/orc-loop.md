# `orc loop` — the cron-shaped bootstrap loop runner

The supervision half of the bootstrap self-improvement loop (spec
[§9.36](../spec/10-orchestrator.md) watch daemon, thin slice; issue #314). `orc loop`
is a foreground runner: poll the board → run one campaign pass (via
[orc campaign](orc-campaign.md)'s selection, one decision record) → spawn the worker →
supervise the in-flight runs → pace the next pass. It is deliberately NOT the always-on
watch daemon — no adaptive tick, no budget classes beyond the minimal guards, no
`orc backlog` controls, no restart recovery. Those deepen in E8.

```sh
orc loop [--json] [--max-cycles N]
```

## Guard set

Every guard is owner-adjustable at the board (issue #310 set), pinned once in
`WorkerBudgets::bootstrap_defaults()` and shared between the worker and the loop (the
loop hands its validated instance to the worker starter) — the two enforcement layers
can never drift apart:

| Guard | Default | Enforced by |
| --- | --- | --- |
| attempts | 3 per worker run | worker loop (typed failure on exhaustion) |
| re-plans | 2 between attempts | worker loop |
| worker timeout | 45m | worker deadline + supervisor kill (beats or not) |
| concurrency | 2 | loop (cap on supervised slots) |
| stall | 10m | worker-internal check + supervisor beat-staleness kill |
| backoff | 10s·2^n capped at 5m | loop pass pacing (same schedule the worker uses for provider retries) |
| spend | $10/day soft cap | worker records exceedance; loop seals the intake (inert by default — the per-token spend estimate is `0.0` until the cost-ledger lane wires provider pricing in, so no accrual crosses the cap) |
| run budget | 4h | loop (aborts in-flight runs, records them) |

A configuration that weakens a guard (zero concurrency, zero stall timeout, zero
shutdown budget, negative spend cap, zero run deadline) is rejected fail-closed: a
weakened guard is a runaway loop.

## Supervision semantics

- **Stall kill.** The worker emits progress beats at every turn boundary and before
  every tool dispatch. If the supervisor observes no beat within the stall timeout, it
  kills the run and records the task as `stalled` — never a silent retry. The
  supervisor-side window closes exactly the case the worker-internal check cannot see:
  a run wedged inside a hung transport call.
- **Worker timeout.** A run that overstays 45m is killed and recorded `timed-out`,
  beats or not.
- **Arbitration.** When a kill is declared, the final status derives from what the run
  handle returns: a natural completion always wins; a cancellation resolves to the
  recorded kill intent; a panic is a typed `failed` row. A run aborted by the
  fatal-exit sweep (a durable-state failure ended the run) is recorded
  `aborted-shutdown` with the detail `unattributed-drain-abort` — no stop reason was
  declared, so the row never claims a budget cause that did not hold.
- **Spawn failure.** A task that cannot be started (missing fixture, unsupported
  provider, transport build failure) is a per-task outcome, not a run-killer: the row
  is recorded terminal `failed` with the spawner's log-safe reason, the pass is paced
  out, and the loop keeps supervising in-flight work. The failed task is excluded for
  the rest of the invocation (no silent re-selection). A fatal durable-state failure
  (store/clock) aborts and records in-flight workers before the run returns — no row
  is left `running`.
- **Run budget.** The budget bounds the board poll, not just the checks between passes:
  a poll that would outlast the remaining budget is cut off at expiry (the summary
  records `run-budget-exhausted`), and a snapshot that arrives after the budget is
  spent is never planned or started. In-flight runs are drained/recorded as with any
  other terminal stop.
- **Graceful stop.** SIGTERM/SIGINT stop the intake immediately, give in-flight runs a
  five-second window (the tech-stack daemon budget) to finish, then abort stragglers
  and record them (`aborted-shutdown`). A second signal short-circuits the remaining
  grace. A signal arriving during a terminal budget stop's drain (spend cap, run
  budget) preempts that drain on the same budget — the stragglers are aborted and
  recorded as shutdown-aborts. A signal arriving during a board poll interrupts the
  poll instead of waiting it out. Stopping leaves board state intact — board-wins on
  the next run.
- **Terminal stops.** The spend soft cap and the run budget end the invocation with a
  typed stop reason (in the summary JSON); empty-queue no-ops are transient — the loop
  backs off and re-polls. A failed board poll (network blip, rate limit) is transient
  too: it is counted in the summary (`poll-failures`), journaled as `poll-failed`, and
  the next pass is paced out — the loop stays alive and keeps supervising in-flight
  workers. The run budget still bounds the poll while it waits.

## Retry and reselection

One worker run per task per loop invocation. A task that failed, stalled, or was
killed is never silently re-selected during the same invocation; any retry is a fresh
board-driven selection in a later invocation (if the board still lists the task as
Ready, the next invocation will pick it up — the typed run rows are the audit trail).
Cross-invocation suppression is a board/PM decision, not a loop policy.

## State

- Decision records: `<config-dir>/campaign.db` — append-only, shared with
  [orc campaign](orc-campaign.md).
- Run state: `<config-dir>/loop.db` — one row per supervised worker run (invocation,
  decision link, heartbeat — the persisted liveness record updated as the supervisor
  observes progress beats —, terminal status, recorded spend, detail). Concurrency
  counts only workers supervised by the current invocation. Historical rows remain
  unchanged on startup; restart recovery is deferred to the E8 watch daemon.

## Single instance

An advisory file lock on `<config-dir>/loop.lock` rejects a second concurrent
invocation with `loop-already-running`. The lock is held by the OS for the process's
lifetime and released on exit or crash — a stale lock can never wedge the next run.
