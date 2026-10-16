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

### Kick-off conditions (§9.36)

The daemon spawns a campaign session when none is in flight and at least
one kick-off condition holds. The five conditions (spec §9.36) are
operator-configurable through the `[watch.kickoff]` layered block; the
built-in defaults ship as TOML data (§9.22.1), never compiled constants:

```toml
[watch.kickoff]
new_eligible_work = true      # ready-queue work the previous pass did not see ready
unblocked_promotion = true    # a blocked task became unblocked (§9.40 promotion)
slot_released = true          # a lease or worker slot was released
budget_window_reset = true    # a per-day budget window reset (§9.19.6)
external_event = false        # an external GitHub event arrived (§9.50)
```

Notes:

- An absent `[watch.kickoff]` block (or absent key) inherits the built-in
  default shown above. Any higher layer (user, org, project, directory)
  overrides any key.
- `external_event` ships **disarmed**: the §9.50 GitHub event ingress is
  opt-in (`[watch.ingress].enabled`, below). With the ingress disabled
  the condition can never fire. Arm it only together with the ingress.
- **Disabling every condition yields an idle-but-healthy daemon**: the
  tick keeps polling and supervising, run state stays durable and
  reconciled — but no campaign session is ever kicked off. Every skipped
  pass persists exactly one typed no-op decision record (the rationale
  carries the kick-off verdict) and journals a `kickoff-skipped` event in
  the run summary — never a silent drop.
- **Budget stops are not a condition.** The budget gates (daily spend
  soft cap, whole-run budget) run before any kick-off evaluation and
  produce the explicit `blocked`/`needs-human` state (§9.33.4) — kick-off
  never bypasses a budget to keep the loop moving. Both budget stops end
  one daemon invocation, not the daemon: the invocation loop restarts on
  `RunBudgetExhausted` immediately, and on `SpendSoftCap` only after
  waiting for the next UTC day boundary (racing the operator's shutdown
  signal) — the spend stays at/above the cap for the rest of the day, so
  an immediate restart would spin, and only a post-boundary invocation's
  first pass can observe the §9.19.6 budget-window-reset condition. Only
  a shutdown signal (or a signal racing the wait) ends the process.
- While a campaign is in flight with free capacity, the active session's
  scheduling continues under the concurrency cap; the kick-off evaluation
  gates starting a session from idle, not the in-flight session's own
  spawn decisions.

## Event ingress (§9.50(b), opt-in)

The daemon can consume external GitHub activity — new issues and
issue/PR comments, plus CI completions (check-run and workflow-run
completions for head SHAs the daemon actively tracks, below) — as
kick-off acceleration, without a human translating it into board state.
(PR ready-for-review is planned but not yet tracked: the repository
events feed this poller consumes does not carry that event.) The shipped
mechanism is the POLL
fallback only: a conditional-request poller over the GitHub REST API
(the §9.50(a) Actions push relay stays blocked until Arbitraitor's
ingress-envelope authentication lands). The §9.43 reconcile poll stays
the completeness authority either way: an event only makes the next
kick-off evaluation fire sooner; a missed or delayed event never loses
work.

```toml
[watch.ingress]
enabled = true           # default false — the operator opts in
queue_capacity = 256     # bounded queue (§7.3); zero is rejected
```

When enabled, the daemon polls each configured board repository's
repository-events feed (`GET /repos/{owner}/{repo}/events`) for issue
opened and issue/PR commented activity, with `If-None-Match`
conditional requests (a 304 costs no quota), honors the
server's `X-Poll-Interval`, and adapts the cadence to rate-limit
feedback — the poll never widens beyond it. Events deduplicate on a
durable delivery key — the platform delivery id, repository, event type,
and head SHA where present — with a 30-day redelivery window
(`<config-dir>/ingress-events.db`), so a redelivery never produces a
second kick-off.

**CI completions lane.** The repository events feed carries no CI
completions (no `CheckRunEvent`, no `WorkflowRunEvent`), so a second
conditional poll lane covers them. The daemon polls
`GET /repos/{owner}/{repo}/actions/runs` (filtered to
`pull_request`-triggered runs) per repository — each completed run is
an event keyed on run id + attempt + conclusion — and
`GET /repos/{owner}/{repo}/commits/{sha}/check-runs` for each head SHA
it actively tracks: every in-flight `pull_request`-triggered run
carries the head SHA the daemon is waiting on, discovered from the same
runs response, bounded (at most 16 tracked SHAs per repository) and
pruned once its CI goes quiet. Check-run completions key on the
check-run id + conclusion; a conclusion transition is a new delivery —
the transition is exactly the signal — while a repeated poll of an
unchanged run deduplicates inside the 30-day window. The lane shares
the events lane's ETag store, dedup table, bounded queue, and cadence
rules; CI state it misses still arrives on the next reconcile pass —
the lane is acceleration, never a trigger authority.

The ingress queue is bounded: sustained flooding drops surplus events
and records the `degraded` marker (surfaced in the log; §7.3 — the bound
exists so a flood hits the drop, never unbounded growth). The marker
clears only after a flood-free window. A dropped event is never lost
work: the board state the reconcile poll reads still describes it.

The ingress alone does not arm the kick-off condition — enable
`[watch.kickoff].external_event = true` (above) to let drained events
kick off a campaign session. Events carry identifiers only (delivery
key, repository, event type, number, head SHA), never board or PR
content.

**Configuration mismatch warnings (issues #504, #603).** `orcd watch`
emits a `tracing::warn!` at startup for each direction of the
ingress/arm mismatch so operators can grep the daemon log and see
which side of the pair is wrong:

- **Forward mismatch** (ingress enabled, condition disarmed):
  `watch.ingress.enabled is true but watch.kickoff.external_event is
  false - the ingress queue has no kick-off consumer and will fill,
  drop surplus, and stay degraded; arm external_event or disable the
  ingress`. The poller still runs — polling is the operator's explicit
  opt-in and the reconcile poll remains the completeness authority
  (gen-13 review finding); the warning is purely informational.
- **Reverse mismatch** (condition armed, ingress disabled):
  `watch.kickoff.external_event is true but watch.ingress.enabled is
  false - the external-event kick-off condition is armed but the
  ingress that feeds it is disabled, so it can never fire; enable the
  ingress or disarm external_event`. Without this warning the operator
  was armed against a trigger source that does not exist (#603).

Consistent pairs (both enabled or both disabled) are silent.

## Bug-report intake (§9.52(a), opt-in)

The daemon can attach external bug reports to the board automatically —
the §9.52(a) intake attach lane. When enabled, the lane consumes the
ingress queue's `issues-opened` events (so it requires the ingress to be
enabled) and attaches each external issue to the configured board with
MECHANICAL FIELDS ONLY:

```toml
[watch.intake]
enabled = true           # default false — the operator opts in
```

The mechanical field set (spec `10-orchestrator.md` §9.52(a)) derives
from LAUNCH FACTS ONLY — the author identity and the event type; issue
titles, bodies, and labels are never read:

- `Type = Bug` (native issue type),
- `Status = Triage`, always — never `Ready` (the trust asymmetry:
  attachment is automatic, scheduling never is),
- `Priority = P1` provisional (never the scheduling value),
- `Target` unset — external reports' MVP-ness is unknowable at attach
  time, so the MVP-only ready-queue predicate excludes the item until
  the triage classification pass (slice 2 of epic #624) sets it;
- a daemon-owned author-class provenance label:
  `intake:human`, `intake:bot`, or `intake:agent`.

An author in the configured `service_identities` set (§9.41 — the same
set the ready queue's assignee exclusion reads) is `intake:agent`-class
and NEVER attaches: the loop's own reports flow the internal paths. An
event whose author launch facts are missing (the platform supplied no
author) fails closed — it attaches nothing rather than guessing a class.
Attachment is idempotent (§9.26.3): one board item per source issue; a
re-delivered event updates rather than duplicates. The lane consumes
its OWN durable intake store (`<config-dir>/intake-events.db`), which
the ingress poller mirrors every accepted `issues-opened` event into:
the kick-off source's consumption of the shared queue can never steal
an attach, a dropped queue surplus is not a lost attach (the durable
store still holds the event), and consumption is delete-on-take — an
event is processed exactly once. A FAILED board write is re-put and
retries on the next cadence tick. The §9.43 reconcile poll stays the
completeness authority. An intake outage degrades nothing else. An
enabled lane over a disabled
ingress is dead configuration and warns at startup (the same
observable-warning discipline as the ingress/kick-off pair).

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
- Event ingress (when `[watch.ingress].enabled`): `<config-dir>/ingress-events.db`
  — the durable dedup table (30-day redelivery window) and the
  per-endpoint conditional-request bookkeeping (ETag, advised poll
  interval) for the events feed, the CI completions lane's check-runs
  endpoints (one per tracked head SHA), and the Actions-runs endpoint.
- Intake store (when `[watch.intake].enabled`): `<config-dir>/intake-events.db`
  — the durable per-event consumption surface the §9.52(a) lane takes
  from (delete-on-take; untaken events persist across restarts).
- Instance lock: `<config-dir>/loop.lock` — shared with `orc loop`.
- Worker fixtures and task worktrees: `<config-dir>/worker-tasks/` and
  `<config-dir>/loop-worktrees/` — the same layouts and branch scheme
  (`orc-loop/<task-id>`) as the foreground loop, pruned at process start
  and before every subsequent run-budget invocation (so a retried task's
  worktree path is always free).
