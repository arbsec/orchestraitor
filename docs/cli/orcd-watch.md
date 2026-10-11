# `orcd watch` — the watch daemon's running mode

The always-running supervision loop for [campaigns](orc-campaign.md) and
their workers (spec [§9.36](../spec/10-orchestrator.md) watch daemon, thin
slice; issue #503). `orcd watch` runs the same poll/supervise cycle as
[`orc loop`](orc-loop.md) — poll the board → run one campaign pass →
supervise the in-flight workers → pace the next pass — as the daemon's
running mode, adding the three things the foreground runner defers:

- **Poll-tick cadence.** The daemon polls the board on a fixed default
  cadence (60 seconds), operator-configurable through the layered config
  (§9.22). Every tick is a reconcile pass (below).
- **Board-wins reconcile (§9.43).** Where local durable state and the board
  disagree, the board wins and the divergence is recorded as a
  `board-diverged` event in the daemon event store
  (`<config-dir>/watch-events.db`). A blocked task whose last blocker
  landed is promoted at the next tick (§9.40) and recorded as an
  `unblocked-task-promoted` event.
- **Crash-safe restart recovery (§9.24.2).** On startup, before the first
  tick, every `loop.db` row still `running` transitions to `orphaned` —
  never directly `failed`, so the work stays re-runnable — and the tick
  resumes from durable state. A `paused` row stays paused.

```sh
orcd watch
```

## Operating modes

Foreground `orc loop` remains a valid, documented operating mode (spec
§9.36: "foreground and systemd-user-unit supervision are documented
operating modes"). The two share one single-flight ownership:

- Both hold the same advisory lock on `<config-dir>/loop.lock`.
- `orcd watch` refuses to start when another `orc loop` or watch instance
  holds the lock, and vice versa.
- The lock is released by the OS on exit or crash — a crashed daemon can
  never wedge the next run; the next start performs restart recovery and
  resumes.

Run the daemon when you want the loop always-on with a paced poll cadence
and reconcile recording; run `orc loop` when you want one foreground
invocation you supervise yourself (CI, cron, a terminal).

## Configuration

The cadence is operator-configurable through the same layered
configuration the CLI reads (`user.toml`, `org.toml`,
`orchestraitor.toml`, `dir.toml`; §9.22):

```toml
[watch]
poll_interval_secs = 300   # default 60; zero is rejected fail-closed
```

The pinned guard set (concurrency 2, stall 10m, worker timeout 45m,
backoff 10s·2^n capped 5m, $10/day spend soft cap, 4h run budget) is
unchanged from [`orc loop`](orc-loop.md#guard-set) — the daemon hands the
same validated `WorkerBudgets` instance to the worker starter, so the two
enforcement layers cannot drift. A zero cadence is a runaway poll loop and
is rejected at startup.

## Supervision semantics

Stall kills (beat staleness over the stall window), worker-timeout kills,
kill arbitration (natural completion wins over a declared kill), spawn
failures as per-task outcomes, the spend-intake seal, graceful shutdown
(SIGTERM/SIGINT with the five-second daemon budget), and the typed run
rows all behave exactly as documented for
[`orc loop`](orc-loop.md#supervision-semantics). The daemon adds:

- **Restart recovery.** A row the supervisor never reached a terminal
  status for reads `orphaned` after restart (detail
  `restart-recovery: supervisor did not reach a terminal status`). Crashes
  transition to `orphaned` — never directly `failed` (§9.24.2). A lease
  expiry detected mid-run (beat staleness) is recorded `stalled` and
  counts against the task retry budget, as in `orc loop`.
- **Failed polls and no-op passes cost time.** The next poll waits for
  the LATER of the cadence and the no-spawn backoff: a transient poll
  failure (network blip, rate limit) waits the full interval before the
  retry, and repeated no-op passes can delay polls up to the 5m backoff
  cap — beyond the configured cadence (the rate-limit courtesy §9.36
  asks of the poller). Supervision continues on every tick either way,
  so in-flight workers are unaffected.
- **Reconcile events.** Every tick's reconcile records its NEW observations
  once per invocation (duplicates within an invocation are suppressed):
  `board-diverged` when a row orphaned by this invocation's restart
  recovery is no longer open on the board (board wins over a crashed
  run; live slots are never flagged), `unblocked-task-promoted` when a
  previously blocked candidate appears on the ready queue. A tick with
  no new observations records nothing; a record failure surfaces as a
  poll failure and the pass does not plan. Events carry identifiers
  only — never board content.

## State

- Run state: `<config-dir>/loop.db` — the same store `orc loop` uses;
  recovery, history, and the daily-spend scan are shared between the two
  operating modes.
- Decision records: `<config-dir>/campaign.db` — unchanged.
- Reconcile events: `<config-dir>/watch-events.db` — append-only,
  hash-chain validated (`orchestraitor-events`).
- Instance lock: `<config-dir>/loop.lock` — shared with `orc loop`.
- Worker fixtures and task worktrees: `<config-dir>/worker-tasks/` and
  `<config-dir>/loop-worktrees/` — the same layouts and branch scheme
  (`orc-loop/<task-id>`) as the foreground loop, pruned at process start
  and before every subsequent run-budget invocation (so a retried task's
  worktree path is always free).
