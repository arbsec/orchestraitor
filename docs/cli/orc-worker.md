# `orc worker` — headless one-shot bootstrap worker

Headless one-shot mini-worker (mini-swe-agent pattern) for the bootstrap milestone
(spec [§9.38](../spec/10-orchestrator.md) MCP-early tool strategy context,
[MVP-6](../spec/60-milestones.md) built-in coding tools): a task description goes in, a
bounded loop of model calls and tool dispatches runs, and a structured result plus
exit code comes out. One-shot headless only — no interactive mode, no sub-agents, no
model self-selection (routing stays with the control plane, see
[orc-routing](orc-routing.md)), no review behavior.

```sh
orc worker run --task <id> [--json]
```

`--task <id>` resolves a fixture task from `<config-dir>/worker-tasks/<id>.json`
(`{"id", "slug", "description"}`); task loading is pluggable — the DAG/backlog
integration replaces the fixture source in a later lane. `--project-dir` is the task
worktree the file tools confine to. Exit code: `0` on success, non-zero on typed
failure; with `--json` the structured result is printed in both cases (typed failures
carry `failure.class` + `failure.reason`).

## Toolset: exactly four tools

| Tool | Behavior |
| --- | --- |
| `read_file` | Reads a worktree-relative file (UTF-8, ≤ 1 MiB). |
| `search` | Plain-substring content search over the worktree (sorted, capped; `.git` and symlinks skipped). MCP-server-based search is a later lane. |
| `bash` | Mediated execution through the Arbitraitor boundary only: the #311 preflight (`MediatedWorker::spawn`) gates the first use and `run_bash` routes through `arbitraitor-exec` under a network-denied policy (see [sandbox-mediation](../sandbox-mediation.md)). Unavailable mediation fails closed with a typed refusal — no non-secure fallback. |
| `write_file` | Bounded (≤ 1 MiB), path-validated write into the task worktree. Absolute paths, `..` escapes, and symlink components are refused. Writes are recorded as untrusted output (spec `40-arbitraitor-integration.md` §9.14); classification and promotion stay with Arbitraitor and the delivery lane. |

Any capability request outside the four is a typed refusal — recorded in the run's
receipt log (`reason: "unknown-tool"`) and fed back to the model; the loop continues
within budget. Every tool call produces a receipt carrying static reason codes only —
never arguments, command lines, or captured output (spec §9.23.4).

## Re-plan feedback

A failed attempt's static failure reason still opens the re-plan prompt (the class
is still useful). Beside it, the re-plan prompt carries a bounded summary of what
that attempt already DID (issue #657): per-tool dispatch counts with outcome
splits, the worktree-relative paths already written ("do not re-read them from
scratch"), and the last tool outcome before the failure — so a re-plan stops
re-reading the same files. The summary renders from the attempt's receipt log
as accumulated across all attempts SO FAR (the executor's log is cumulative —
a second replan summarizes everything the first two attempts did), with its
body capped at 2048 characters (the labelled framing, the fences, and
the truncation notice add a small fixed amount) inside a fenced, labelled
UNTRUSTED-DATA block
(spec `40-arbitraitor-integration.md` §6.1; the fence is widened over any
backtick run so the content can never close its own fence), with an explicit
truncation NOTICE when the cap cuts the payload. An attempt that recorded
neither tool calls nor writes contributes no summary block.

## Budgets

Owner-adjustable (issue #310 bootstrap values):

| Budget | Value | Enforcement |
| --- | --- | --- |
| attempts | 3 | Typed `attempt-budget-exhausted` failure; never an infinite retry. |
| re-plan | 2 | Typed `replan-budget-exhausted` failure. |
| worker timeout | 45m | Typed `worker-timeout` failure. |
| concurrency | 2 | Scheduler-facing bound; one-shot `orc worker run` executes one worker. |
| stall | 10m | Typed `stalled` failure when a turn window produces no tool progress. |
| backoff | 10s·2^n capped 5m | Provider-call retries between model calls. |
| spend | $10/day | **Soft cap**: exceeding is recorded on the result (`spend_soft_cap_exceeded`), never a hard stop. |
| run budget | 4h | Hard wall-clock bound combined with the worker timeout (whichever elapses first). |

## Result JSON

`orc worker run --json` emits the structured result: `task_id`, `status`
(`completed`/`failed`), `exit_code`, `summary`, `failure`, `delivery`, `attempts`,
`replans`, `turns`, `model_calls`, `usage`, `spend_soft_cap_exceeded`,
`untrusted_writes`, `receipts`, and the effective `budgets` echo. Delivery is a seam:
the default sink reports `delivery.kind = "pending"`; with `--deliver` the
real sink reports `delivery.kind = "pr-opened"` with the PR reference (below).

## Delivery (`--deliver`)

With `orc worker run --deliver --delivery-repo <owner/name>` (issue #656), a
COMPLETED run's worktree state lands through the App-signed landing primitive
and a PR opens:

1. The worktree's current state is staged and committed onto the run's
   checked-out branch.
2. `orc github push-branch` lands the branch's committed tree as ONE
   App-signed commit on `orc-delivery/<task-id>` (the branch the worktree
   has checked out is verified; dirty-vs-committed distinction holds — the
   landing carries the committed tree after the step-1 commit).
3. `orc github gh-env-pr-create-chain` opens the PR (head
   `orc-delivery/<task-id>`, base `--delivery-base`, default `main`) with
   the run summary as the body — through the ONLY sanctioned `gh pr create`
   route, so the deterministic attribution/labeling chain runs.

Every delivery failure is a TYPED outcome on the run result (the run fails
with `failure.class = "delivery-failed"` and a static reason code, spec
§9.23.4): `delivery-no-worktree` (run carried no worktree/branch context),
`delivery-branch-state-failed` (staging/committing or branch verification
failed), `delivery-push-failed` (the landing failed),
`delivery-empty-diff` (the committed tree is identical to the base —
nothing to land), `delivery-pr-open-failed` (PR creation failed or
returned no reference). A push failure is never a silent drop, and never
silently blocks the loop's success accounting: the loop's run row carries
the typed failure. Untrusted-write classification/promotion stays with
Arbitraitor (spec §9.14); the sink performs no classification.

`orc loop` arms delivery per invocation with `--deliver
[--delivery-repo <owner/name>] [--delivery-base <branch>]` (repo defaults
to the first board-config repo); `orcd watch` arms it through environment
configuration: `ORCHESTRAITOR_DELIVERY_REPO=<owner/name>` (arming key —
absent keeps the typed `Pending` outcome), `ORCHESTRAITOR_DELIVERY_BASE`
(default `main`), `ORCHESTRAITOR_DELIVERY_ORC_BIN` (default: the `orcd`
executable itself). Misconfiguration is a typed startup error, never
discovered after a task completes.

## Test-only overrides

- `--worker-tasks-dir <dir>` (env `ORCHESTRAITOR_WORKER_TASKS_DIR`) — alternate fixture
  task directory.
- `--worker-provider-endpoint <url>` (env `ORCHESTRAITOR_WORKER_PROVIDER_ENDPOINT`) —
  alternate provider base URL; used by the deterministic simulator in tests (spec
  §21.3 — CI never depends on a live provider).

## Rollback

The worker is a new binary path; no existing command behavior is modified.
