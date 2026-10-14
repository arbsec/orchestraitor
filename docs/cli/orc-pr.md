# `orc pr`

Read-only pull-request status surfaces (spec
[`10-orchestrator.md`](../spec/10-orchestrator.md) §9.48). The `orc pr`
subcommand tree exposes the §9.48 PR-status aggregation engine — the same
typed verdict the `pr.status` coordinator decision tool (§9.39) serves.

## `orc pr status <N>`

Answers ONE question — "is PR N mergeable right now, and if not, why not?" —
from a single invocation. The engine reproduces, in-process, the semantics of
the five pr-lifecycle skill scripts it replaces for agents (`.agents/skills/
github-pr-lifecycle/scripts/`: `pr-checks`, `review-threads`,
`reconcile-checklist`, `convergence-status`, `merge-gate`): one PR view, one
checks rollup, one `reviewThreads` GraphQL query, one body fetch, one
CLEAN-record file read. The scripts stay the shell-level operators' surface
and the normative definition of the classification rules; the engine consumes
their semantics verbatim — one vocabulary, never a second interpretation.

```sh
orc pr status 614 --repo arbsec/orchestraitor            # human one-pager
orc pr status 614 --repo arbsec/orchestraitor --json     # typed §9.48 JSON
```

- **`--repo owner/name`** targets the repository explicitly; without it the
  `origin` remote of the current checkout is used (never guessed beyond that).
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
