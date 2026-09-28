# `orc campaign` — one-shot campaign pass

The manager half of the bootstrap self-improvement loop (spec
[§9.35](../spec/10-orchestrator.md) campaign orchestration: session-per-decision,
[M1](../spec/60-milestones.md) live self-referential loop). One campaign pass is a
fresh, short-lived invocation: it reads the reconciled board state, applies the minimal
epic-focus rule (items whose configured priority field carries `P0` first, stable
`(repo, issue-number)` order otherwise), selects at most one eligible task, persists exactly one append-only decision
record, and — for a selection — spawns the worker on the daemon-less direct path (the
same seams as [orc worker](orc-worker.md)). One-shot only in this slice: the cron-shaped
loop runner (`orc loop`) arrives with the bootstrap-loop task; the always-running watch
daemon is E8.

```sh
orc campaign run --once [--json]
```

Without `--once` the command fails with a typed error naming the loop lane. Exit code:
`0` on a no-op pass or a completed worker run. Non-zero on a typed failure: board,
routing, and store failures persist no record; a post-selection worker failure (typed
failure inside the run, or a spawn that produced no run at all) exits non-zero with the
decision record already persisted.

## Selection

The pass consumes the ready queue exactly as `orc board ready` computes it (leaf
Task/Bug, `Target=MVP`, `Status=Ready`, no unresolved blockers, fail-closed on
truncated windows). It then orders the queue: items whose configured priority field
(board config `priority_field`, default `Priority`) carries `P0` come first, and the
rest keep stable issue-number order. The first item is selected; every other
ready item is recorded as an alternative. Selection is deterministic: the same board
yields the same record fields.

The worker task id is derived deterministically from the board identity as
`board-<owner-repo>-<number>` (issue numbers are per-repo, so the id carries the repo)
and resolves a fixture task from `<config-dir>/worker-tasks/<id>.json` (see
[orc worker](orc-worker.md)).

One invocation at a time: single-flight is owned by the loop runner / watch daemon
(later bootstrap lanes), so concurrent `orc campaign run` invocations may select the
same first item and each dispatch its own worker against the same worktree.

## No-op passes

A pass that cannot select still persists its one record with a typed reason:

| Reason | Meaning | Worker |
| --- | --- | --- |
| `empty-queue` | No eligible work exists: no open items, or open items that are not Ready-status candidates. The record discloses unevaluable (fail-closed) items. | not spawned |
| `all-blocked` | Eligible candidates exist whose only disqualifier is unresolved blockers; the record carries the blocked graph (each candidate with its unresolved-blocker count and board field values). | not spawned |
| `epic-exhausted` | Every tracked item is closed. | not spawned |

## Decision records

Records are append-only local state at `<config-dir>/campaign.db` (SQLite,
`schema_migrations`-versioned). Each row stores the typed payload (kind, selected task,
role, provider, model, precedence path, fallback reason, worker argv, rationale,
alternatives, blocked graph) for replay; a bad pass is re-runnable fresh. The store
holds decisions only — no board content beyond the inert titles the record already
carries, never credentials or run output.

With `--json` the command prints the persisted decision plus the worker result, when
one ran.
