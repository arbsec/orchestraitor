# `orc campaign` — one-shot campaign pass

The manager half of the bootstrap self-improvement loop (spec
[§9.35](../spec/10-orchestrator.md) campaign orchestration: session-per-decision,
[MVP-1](../spec/60-milestones.md) live self-referential loop). One campaign pass is a
fresh, short-lived invocation: it reads the reconciled board state, applies the minimal
epic-focus rule (P0-labelled items first, stable `(repo, issue-number)` order
otherwise), selects at most one eligible task, persists exactly one append-only decision
record, and — for a selection — spawns the worker on the daemon-less direct path (the
same seams as [orc worker](orc-worker.md)). One-shot only in this slice: the cron-shaped
loop runner (`orc loop`) arrives with the bootstrap-loop task; the always-running watch
daemon is E8.

```sh
orc campaign run --once [--json]
```

Without `--once` the command fails with a typed error naming the loop lane. Exit code:
`0` on a no-op pass or a completed worker run; non-zero on a typed failure (board,
routing, store, or worker), with the decision record still persisted.

## Selection

The pass consumes the ready queue exactly as `orc board ready` computes it (leaf
Task/Bug, `Target=MVP`, `Status=Ready`, no unresolved blockers, fail-closed on
truncated windows) and orders it P0-first. The first item is selected; every other
ready item is recorded as an alternative. Selection is deterministic: the same board
yields the same record fields.

The worker task id is derived deterministically from the issue number as
`board-<number>` and resolves a fixture task from
`<config-dir>/worker-tasks/<id>.json` (see [orc worker](orc-worker.md)).

## No-op passes

A pass that cannot select still persists its one record with a typed reason:

| Reason | Meaning | Worker |
| --- | --- | --- |
| `empty-queue` | No open board items in the configured repositories. | not spawned |
| `all-blocked` | Open items exist but none satisfy the ready predicate; the record carries the blocked graph (each open item with its unresolved-blocker count and board field values). | not spawned |
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
