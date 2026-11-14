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
- **Known-disabled and structural skips.** The `pr-checks` `KNOWN_DISABLED_JOBS`
  semantics are consumed verbatim: a known-disabled job ("Documentation", the
  push-gated docs build; `Docs Consistency` is a pull-request job and therefore
  NOT in the set) exempts from the all-passed gate only while its bucket is
  `skipping`; the same job actually failing is a failure, never a pass. A
  `CodeRabbit` `skipping` check on a PR authored by a bot/App login
  (`<slug>[bot]` or `app/<slug>`) is the structural bot-author no-op
  (`skip_reason: "bot-author"`): it is `not-applicable` and never fails
  `checks_all_passed`. A skip on a human/unknown-authored PR, or a check the
  engine cannot classify, stays non-passing (fail closed).

### The verdict

The `--json` output is the §9.48 schema: `number`, `repository`, `head_sha`,
`base_oid` (the two convergence anchors), `is_draft`, `review_decision`,
`merge_state`, classified `checks` (name, state, bucket, classification,
`skip_reason`, workflow, link) with the aggregate booleans `checks_all_passed`,
`checks_any_failing`, `checks_any_pending`, `threads` (total, unresolved
actionable, resolved, outdated, size-capped items), `checklist`
(`marker_count`, `markers_present`, `all_checked`, `unchecked_count`,
and the `<!-- orc:* -->` items), `review_record`
(the `.orchestraitor/reviews/`
CLEAN-record state), `convergence` (`mergeable` + `reasons`), and
`fetched_at`. `all_checked` is true only when at least one marker exists AND
every marker is checked: a body with no managed markers is MISSING evidence
(reason `checklist_markers_missing`), never a vacuous pass. There is NO
author-granted opt-out — the body cannot waive itself, so a marker-less body
always blocks. A marker inside a fenced code region (3+ backticks or 3+
tildes) or inside an HTML comment (`<!--` … `-->`) is an EXAMPLE, not a
checklist item, and is excluded from the scan. A fence line carries at most
three leading spaces (four or more, or a leading tab, is an indented code block
where fence characters are literal text), and a backtick fence's info string may
not contain a backtick — ```` ```example``` ```` is a closed inline code span,
not an opener — while a tilde fence's may. A CLOSING fence may be followed ONLY
by spaces or tabs (CommonMark 0.31). A form feed or a vertical tab after the
closing run is CONTENT: it leaves the region OPEN, so a marker behind such a
closer is still inside the fence. Line endings are normalised ONCE before any
scan (CommonMark §2.1: a lone CR is a line ending and so is CRLF; both become
LF), so a closing fence followed by a CRLF-CRLF terminator closes. Only a space
or a tab indents a checklist item. An HTML comment opens only when `<!--` is the
line's first non-whitespace content (at most three leading spaces) — a `<!--`
inside an inline code span or after prose is literal text, so the rest of that
line stays visible — and closes at the first line carrying `-->` (the opener line
included), while an unclosed comment runs to the end of the body. A fence OPENER
indented 1-3 spaces has an
INDETERMINATE container scope (it may belong to a list item or a block quote,
and the scanner has no container model): the checklist is not read, `all_checked`
is false, and the verdict blocks with the distinct reason
`checklist_unparseable` (arbsec/orchestraitor#809). The condition is sticky —
closing the region does not clear it — and the one remedy is to dedent the fence
opener to column 0.

A managed marker has ONE canonical form: `[ \t]*- [BOX] <!-- orc:NAME -->`, where
every separator is EXACTLY one ASCII space (between `-` and `[`, between `]` and
`<!--`, and inside `<!-- orc:NAME -->`), `<BOX>` is exactly one of `x`, `X` or a
single space, and `<NAME>` is one of the nine managed names (`issue`,
`service-identity`, `tests`, `security`, `docs`, `rollback`, `checks`, `review`,
`threads`). A line that is not canonical but carries BOTH a task-item box token
(`[` + exactly one character + `]`) AND a reference to one of the nine managed
names (`orc:NAME`) is a **near-marker**: the gate cannot tell what the author
asserted, so the checklist is not read, `all_checked` is false, and the verdict
blocks with `checklist_unparseable` (arbsec/orchestraitor#799 R1). A line that
only mentions `orc:NAME` (prose) and a line that carries only a box token (a
plain task) are not near-markers and never block. A RAW HTML BLOCK is an
invisible region as well: its content is raw HTML, so a task item inside it is
not a checkbox and backticks inside it are not a fence — the block opens on a
line whose first content is `<` followed by an ASCII letter, closes at the first
subsequent line carrying `</tag>` for the type-1 tags `pre`, `script`, `style`
and `textarea` and at the next blank line for every other tag.

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
