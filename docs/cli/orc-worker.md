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
the bootstrap sink reports `delivery.kind = "pending"` — worktree/commit/push/PR
wiring is a later lane; tests substitute a fixture sink that returns a PR reference.

## Test-only overrides

- `--worker-tasks-dir <dir>` (env `ORCHESTRAITOR_WORKER_TASKS_DIR`) — alternate fixture
  task directory.
- `--worker-provider-endpoint <url>` (env `ORCHESTRAITOR_WORKER_PROVIDER_ENDPOINT`) —
  alternate provider base URL; used by the deterministic simulator in tests (spec
  §21.3 — CI never depends on a live provider).

## Rollback

The worker is a new binary path; no existing command behavior is modified.
