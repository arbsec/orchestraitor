# Bootstrap Loop Quickstart

> **Landing with [#434](https://github.com/arbsec/orchestraitor/pull/434).** The `orc loop`
> bootstrap runner is not on `main` yet. This page describes how you adopt it once that PR
> merges; every command, flag, status, and guard below is drawn from the PR's reference
> documentation (`docs/cli/orc-loop.md` and its sibling pages), not from shipped behavior.

`orc loop` is the supervision half of the bootstrap self-improvement loop: a foreground
runner that polls the board, runs one campaign pass, spawns the worker, supervises the
in-flight runs, and paces the next pass. It is deliberately not the always-on watch daemon —
there is no adaptive tick, no budget classes beyond the minimal guards, no `orc backlog`
controls, and no restart recovery; those deepen in the E8 milestone.

```sh
orc loop [--json] [--max-cycles N]
```

## Prerequisites

- **A configured board.** Copy
  `.agents/project/github-project.example.toml` to
  `.agents/project/github-project.local.toml` and describe the shared GitHub Projects v2
  board with human-readable names (the example file is documentation only and is never
  loaded). The command searches upward from the project directory, so it works from anywhere
  inside the checkout. GitHub node IDs are never stored in config; they are resolved at
  runtime via GraphQL and cached under `$XDG_CACHE_HOME/orchestraitor/`.
- **Board authentication.** Auth is explicit — no ambient credential sniffing. Until the
  GitHub App service-identity module lands, set a bootstrap token URI in the local board
  config:

  ```toml
  [auth]
  token = "secret://env/GH_TOKEN" # any env var name; e.g. export GH_TOKEN="$(gh auth token)"
  ```

  The token is held in a `secrecy::SecretString`, injected only into the `Authorization`
  header of GraphQL requests, and never appears in errors, logs, or output.
- **Optional service identity.** The intended long-term identity is the org-owned
  `arbsec-agent` GitHub App. Layered config keys `service_identities` and
  `[github_app]` (`slug`, `client_id`, `installation_id`, `private_key_uri`) are documented
  in [`docs/cli/orc-github.md`][orc-github]; until the App is registered, personal owner
  auth is the labelled bootstrap fallback above.
- **Worker task fixtures.** The bootstrap worker resolves its task from
  `<config-dir>/worker-tasks/<id>.json` (`{"id", "slug", "description"}`), where
  `<config-dir>` defaults to `.orchestraitor/`. The loop derives the id deterministically
  from the board item as `board-<owner>_<repo>-<number>`, so a selected issue needs a
  matching fixture file before its pass can run the worker.
- **Worker role routing.** Spawned workers run as the `implement` role; routing must
  resolve through the layered configuration. The shipped built-in default (Neuralwatt
  GLM-5.2, key via `secret://keyring/neuralwatt` or `NEURALWATT_API_KEY`) resolves out of
  the box — see `docs/cli/orc-routing.md`.
- **A reachable board.** Verify the whole read path before starting the loop:

  ```sh
  orc board ready --json
  ```

  This exercises the same reconciled board read the loop's poll uses. It should list open
  leaf Task/Bug items with `Target=MVP`, `Status=Ready`, and no unresolved `blockedBy`
  edges.

[orc-github]: https://github.com/arbsec/orchestraitor/blob/main/docs/cli/orc-github.md

## First invocation

```sh
orc loop --max-cycles 3 --json
```

`--max-cycles N` stops after N board polls — a QA/evidence bound. Without it the loop runs
until a budget stop or a shutdown signal. `--json` prints the end-of-run summary as JSON;
without it the same counts print as text.

Only one loop invocation may run at a time: an advisory file lock on
`<config-dir>/loop.lock` rejects a second concurrent invocation with a typed
`loop-already-running` error. The lock is held by the OS for the process's lifetime and
released on exit or crash, so a stale lock can never wedge the next run.

## What a normal cycle looks like

Each pass:

1. **Poll the board** — the same reconciled read `orc campaign run` does (open items, ready
   queue, blocked candidates, warnings).
2. **Run one campaign pass** — apply the P0-first epic-focus rule (items whose configured
   priority field carries `P0` first, stable `(repo, issue-number)` order otherwise), select
   at most one eligible task, and persist exactly one append-only decision record to
   `<config-dir>/campaign.db`. Every other ready item is recorded as an alternative; a pass
   that cannot select still persists its record with a typed reason (`empty-queue`,
   `all-blocked`, `epic-exhausted`).
3. **Spawn the worker** on the daemon-less direct path, as the `implement` role, with a
   validated instance of the guard set shared with the worker layer.
4. **Supervise** — the worker emits progress beats at every turn boundary and before every
   tool dispatch; the supervisor watches beats, enforces the worker timeout, and persists
   liveness to `loop.db`.
5. **Pace the next pass** — a pass that produced no spawn backs off on the pinned schedule
   (10s·2^n capped at 5m, the same schedule the worker uses for provider retries). An
   empty-queue no-op is transient: the loop backs off and re-polls.

One worker run per task per loop invocation. A task that failed, stalled, or was killed is
never silently re-selected during the same invocation; any retry is a fresh board-driven
selection in a later invocation. Cross-invocation suppression is a board/project-manager
decision, not loop policy.

## How a task flows from board to PR

```text
board item (Task/Bug, MVP, Ready, unblocked)
  → campaign pass selects it, records the decision (campaign.db)
  → worker runs the fixture task (four mediated tools, bounded loop)
  → completed run hands off at the delivery seam
```

The worker is a headless one-shot mini-agent with exactly four tools — worktree-confined
`read_file` and `write_file`, local content `search`, and `bash` mediated through the
Arbitraitor boundary under a network-denied policy. It always runs in the invoking project
directory; the bootstrap slice is single-workspace. In this slice delivery is a seam: the
bootstrap sink reports `delivery.kind = "pending"`, and the real worktree/commit/push/PR
path wires in behind the same `DeliverySink` trait in a later lane. A completed run today
ends at the structured result, not at an opened pull request — the decision and run records
are the audit trail meanwhile.

## Reading run state

Two SQLite stores under `<config-dir>/` (default `.orchestraitor/`):

- `<config-dir>/campaign.db` — append-only decision records, shared with `orc campaign`.
- `<config-dir>/loop.db` — one row per supervised worker run, in the `loop_worker_runs`
  table: `id`, `invocation_id`, `decision_id`, `task_id`, `repo`, `number`, `status`,
  `started_at_secs`, `heartbeat_turn`, `heartbeat_at_secs`, `finished_at_secs`,
  `spend_usd`, `detail`.

Row statuses: `running`, `completed`, `failed`, `stalled`, `timed-out`,
`aborted-shutdown`. A row is mutated at most once from `running` to a terminal status;
`detail` carries the typed reason for terminal rows. Historical rows remain unchanged on
startup (restart recovery is deferred to the E8 watch daemon). For example:

```sh
sqlite3 .orchestraitor/loop.db \
  'select id, task_id, status, detail from loop_worker_runs order by id desc;'
```

With `--json`, the end-of-run summary prints:

- `stop_reason` — one of `run-budget-exhausted`, `spend-soft-cap`, `cycle-budget`,
  `shutdown`;
- counts — `cycles` (board polls), `spawns` (worker runs started), `completed`, `failed`,
  `stalled`, `timed_out`, `aborted_on_stop`, `poll_failures`;
- `elapsed_secs`;
- `events` — the event journal in observation order, tagged in kebab-case (`pass-planned`,
  `worker-spawned`, `worker-finished`, `worker-stall-killed`, `worker-timed-out`,
  `worker-aborted-on-stop`, `poll-failed`, `backing-off`). Events carry identifiers only —
  never board content or task payloads.

Without `--json` the same summary prints as a few text lines (`loop stopped: ...`,
`cycles = N spawns = N`, the outcome counts, `elapsed = Ns`).

## Stalls, timeouts, and budget stops

- **Stall kill.** If the supervisor observes no beat within the stall timeout (10m), it
  kills the run and records it `stalled` — never a silent retry. The supervisor-side window
  closes exactly the case the worker-internal check cannot see: a run wedged inside a hung
  transport call.
- **Worker timeout.** A run that overstays 45m is killed and recorded `timed-out`, beats or
  not.
- **Arbitration.** When a kill is declared, the final status derives from what the run
  handle returns: a natural completion always wins; a cancellation resolves to the recorded
  kill intent; a panic is a typed `failed` row.
- **Run budget.** The 4h run budget bounds the board poll too: a poll that would outlast the
  remaining budget is cut off at expiry, and a snapshot that arrives after the budget is
  spent is never planned or started. In-flight runs are drained and recorded like any other
  terminal stop, and the summary reports `run-budget-exhausted`.
- **Spend soft cap.** The $10/day cap is a soft stop: exceedance is recorded and the loop
  seals its intake, draining in-flight runs naturally (stop reason `spend-soft-cap`). It is
  inert by default — the per-token spend estimate is `0.0` until the cost-ledger lane wires
  provider pricing in, so no accrual crosses the cap.
- **Failed polls are transient.** A failed board poll (network blip, rate limit) is counted
  in the summary (`poll_failures`), journaled as `poll-failed`, and the next pass is paced
  out — the loop stays alive and keeps supervising in-flight workers.

## Stopping the loop safely

Send `SIGTERM` or `SIGINT` (Ctrl-C). The intake stops immediately, in-flight runs get a
five-second window to finish, then stragglers are aborted and recorded as
`aborted-shutdown`. A second signal short-circuits the remaining grace. A signal arriving
during a terminal budget stop's drain preempts that drain on the same budget; a signal
arriving during a board poll interrupts the poll instead of waiting it out.

Stopping leaves board state intact — the board wins on the next run, and unvisited work is
simply picked up by a later invocation. There is no restart recovery of in-flight rows in
this slice: historical rows stay as recorded, and concurrency counting applies only to
workers supervised by the current invocation.

## The guard set you can adjust

Every guard is owner-adjustable at the board (issue #310 set), pinned once in
`WorkerBudgets::bootstrap_defaults()` and shared between the worker and the loop — the two
enforcement layers can never drift apart:

| Guard | Default | Enforced by |
| --- | --- | --- |
| attempts | 3 per worker run | worker loop (typed failure on exhaustion) |
| re-plans | 2 between attempts | worker loop |
| worker timeout | 45m | worker deadline + supervisor kill (beats or not) |
| concurrency | 2 | loop (cap on supervised slots) |
| stall | 10m | worker-internal check + supervisor beat-staleness kill |
| backoff | 10s·2^n capped at 5m | loop pass pacing |
| spend | $10/day soft cap | worker records exceedance; loop seals the intake (inert by default) |
| run budget | 4h | loop (aborts in-flight runs, records them) |

A configuration that weakens a guard (zero concurrency, zero stall timeout, zero shutdown
budget, negative spend cap, zero run deadline) is rejected fail-closed: a weakened guard is
a runaway loop.

The full supervision semantics — including arbitration details and the terminal-stop
vocabulary — live in the command reference,
[`docs/cli/orc-loop.md`][orc-loop] (it lands on `main` with #434).

[orc-loop]: https://github.com/arbsec/orchestraitor/blob/main/docs/cli/orc-loop.md
