# `orc pr`

Read-only pull-request status surfaces (spec
[`10-orchestrator.md`](../spec/10-orchestrator.md) §9.48). The `orc pr`
subcommand tree exposes the §9.48 PR-status aggregation engine — the same
typed verdict the `pr.status` coordinator decision tool (§9.39) serves.

## `orc pr status <N>`

Answers ONE question — "is PR N mergeable right now, and if not, why not?" —
from a single invocation. The engine reproduces, in-process, the semantics of
the five pr-lifecycle skill scripts it replaces for agents
(`.agents/skills/github-pr-lifecycle/scripts/`: `pr-checks`,
`review-threads`, `reconcile-checklist`, `convergence-status`,
`merge-gate`): one PR view, one checks rollup, one `reviewThreads` GraphQL query, one body fetch, one
CLEAN-record file read. The scripts stay the shell-level operators' surface
and the normative definition of the classification rules; the engine consumes
their semantics verbatim — one vocabulary, never a second interpretation.

```sh
orc pr status 614 --repo arbsec/orchestraitor            # human one-pager
orc pr status 614 --repo arbsec/orchestraitor --json     # typed §9.48 JSON
```

- **`--repo owner/name`** targets the repository explicitly; without it the
  `origin` remote of the current checkout is used (never guessed beyond that).
  The CLEAN-review-record read ALWAYS resolves the git toplevel of the current
  working directory (the records live in `<repo-root>/.orchestraitor/reviews/`,
  exactly as `convergence-status` reads them) — run from the target checkout
  even with an explicit `--repo`; outside any checkout the command exits 2.
- **Read-only.** The tool never labels, comments, reviews, or merges.
  `merge-gate` remains the mutating gate; `pr status` reports what it would
  decide without performing it.
- **`merge_state` is informational only.** It is never a convergence input
  (`convergence-status` and `merge-gate` do not read it); a merge refusal
  surfaces from `gh` at merge time.
- **Known-disabled jobs.** The `pr-checks` `KNOWN_DISABLED_JOBS` semantics
  are consumed verbatim: a known-disabled job ("Docs Consistency",
  "Documentation") exempts from the all-passed gate only while its bucket is
  `skipping`; the same job actually failing is a failure, never a pass.

### The verdict

The `--json` output is the §9.48 schema: `number`, `repository`, `head_sha`,
`base_oid` (the two convergence anchors), `is_draft`, `review_decision`,
`merge_state`, classified `checks` (name, state, bucket, classification,
workflow, link) with the aggregate booleans `checks_all_passed`,
`checks_any_failing`, `checks_any_pending`, `threads` (total, unresolved
actionable, resolved, outdated, size-capped items), `checklist`
(`<!-- orc:* -->` markers), `review_record` (the `.orchestraitor/reviews/`
CLEAN-record state), `convergence` (`mergeable` + `reasons`), and
`fetched_at`.

The human one-pager renders the same verdict as verdict-first prose
(`MERGEABLE` / `BLOCKED`), the reason codes, one line per failing/pending
check, one line per unresolved actionable thread, and the checklist delta.

`mergeable: false` is NOT an error — a BLOCKED answer is a successful read.

### Exit codes

| Code | Meaning |
| --- | --- |
| `0` | mergeable (a BLOCKED PR still exits 0 with `mergeable: false` + reasons) |
| `1` | unrecoverable (network, auth, malformed provider output) |
| `2` | config/state (PR not found, missing/partial project config, `github_app` service identity unresolved) |
| `3` | reserved — never used by this read-only tool |
| `4` | concurrent edit: the head SHA or base OID moved between the pre-read and the post-read; re-read and retry |

### Snapshot consistency (exit 4)

The engine captures the PR's head SHA and base OID BEFORE the sub-reads and
re-reads BOTH from the provider AFTER them. If either anchor moved, the
invocation returns exit 4 naming the moved anchor and never a verdict
assembled from a mixed snapshot — the same discipline `merge-gate` applies
between its convergence read and the merge call.

### Fail-closed unread sub-checks (exit 1)

A sub-read that cannot complete (provider 4xx/5xx, malformed payload,
transport failure) records the stable reason code `unread(<sub-check>)`,
reports `mergeable: false` with that reason, emits the typed JSON on stderr,
and EXITS 1 — an unread aggregate is a provider failure and must never
masquerade as a BLOCKED verdict (exit 0). A caller that needs the partial
state reads the emitted JSON; a caller gating on exit codes never treats a
degraded read as a green light.

### Service identity

Reads run through the same `github_app` service-identity mint path every
`orc github` subcommand uses — the installation token is the read credential,
never a personal token. There is no personal-account fallback: a missing or
partial `github_app` config is a typed config error (exit 2). No secrets
appear in the output: the token lives only in the minting client.

## The `pr.status` coordinator decision tool (daemon/MCP surface)

The §9.48 engine is SHARED ("one engine, three surfaces"): the typed verdict,
the pure classification semantics, and the aggregation live in the
`orchestraitor-mcp` crate (`orchestraitor_mcp::pr_status`), consumed verbatim
by BOTH `orc pr status` (this CLI) and the daemon's `pr.status` MCP gateway
tool (spec `10-orchestrator.md` §9.39, §9.48) — the same typed output, the
same reason codes, never a second interpretation. The CLI keeps only its
shell: `--repo` resolution, the service-identity mint, the blocking GitHub
transport, and the human one-pager. The tool is READ-ONLY: it never labels,
comments, reviews, or merges.

**Enabling the tool requires ALL THREE gateway context values** — an
operator who sets only a subset gets a hidden, uncallsable tool (the same
`disable_route` posture as `board.query`), never a partially scoped one:

1. `pr_status` — the GraphQL transport reading through the configured
   `github_app` service identity (the same mint path the CLI uses).
2. `pr_status_repo_root` — the workspace root the tool reads
   `.orchestraitor/reviews/` under (the CLEAN-review-record store).
3. `pr_status_allowed_repo` — the single `owner/name` repository the tool
   may read. Tool arguments are untrusted input (§6.1): a request naming
   any OTHER repository is refused BEFORE the engine runs with the typed
   `pr_status_out_of_scope` payload (naming the configured scope), and the
   refusal is recorded in the audit store with the `out_of_scope` outcome
   class — scope-boundary probes leave an audit trail.

Every invocation — verdict, typed error, or scope refusal — records a
`ToolRequest` event in the shared audit store with the §9.25.1
delegation chain (client-asserted labels, `claimed:`-prefixed and
truncated) and an outcome class (`verdict` / `unrecoverable` / `config` /
`concurrent_edit` / `out_of_scope`). An unread sub-check returns the
typed error with the degraded `partial_verdict` field (§9.48 invariant).

## The `orc chat pr-status` view (§9.44)

`orc chat pr-status <N> [--repo <slug>]` renders the SAME shared-engine
verdict as the §9.44 operator-chat progress read: verdict-first prose
(`MERGEABLE` / `BLOCKED`), the reason codes, one line per failing/pending
check, one line per unresolved actionable thread, the checklist delta, and
the **§9.44 last-synced caveat** — the view names its snapshot instant
(`fetched_at`) and states that it describes durable state AT that instant,
never a live feed; re-run to sync.

- Read-only, no `--json` machine surface: chat views are prose for the
  operator; machines consume `orc pr status --json`.
- A BLOCKED view is a successful read (exit 0); the exit-code contract is
  identical to `orc pr status`.
- The view NEVER caches: every invocation is a fresh aggregation, so the
  caveat names the instant the answer was actually built.

## Reference expansion at the delegation boundary (§9.49)

The orchestration layer expands `pr://<N>` and `issue://<N>` references in
worker-bound task payloads PRE-SEND (spec §9.49): the worker receives the
bounded expanded block and performs ZERO fetches of its own. The expansion
engine lives in the shared mcp crate (`orchestraitor_mcp::ref_expansion`) —
the PR flavor consumes the §9.48 engine's typed output (state, head/base
OIDs, CI booleans, thread counts), never a second vocabulary.

- **Grammar (fail-closed):** only the exact shapes `pr://<digits>` and
  `issue://<digits>` are references. Unrecognized schemes (`issue:/`,
  `pr:/x`, `fixes://`, `cr://…`) pass through as literal text, never
  best-effort expanded.
- **Bounded payload:** 2 KiB per expanded reference, 8 KiB per pass;
  titles/labels size-capped; full bodies, comments, and review text are
  NEVER inlined. The pass budget stops inlining deterministically (later
  references stay literal with a `budget-exceeded` notice).
- **Data, never instructions (§6.1):** expanded content is fenced
  (```` ```expanded pr://N (untrusted content below) ````) with no
  instruction-bearing framing; the fence boundary IS the containment.
- **Secret redaction (§9.23):** token shapes (`ghp_…`, `gho_…`, `github_pat_…`,
  `sk-…`), `secret://` URIs, and Bearer/credential values are REDACTED with
  an announced `redacted (N instance(s))` — never a silent pass.
- **Typed failures, never blocking:** `not-found`, `forbidden`,
  `rate-limited`, `provider-error`, and `budget-exceeded` leave the
  reference as literal text with a typed notice; the payload is still
  delivered. No synchronous retry (§9.26.1).
