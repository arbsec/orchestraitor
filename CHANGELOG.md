# Changelog

All notable consumer-visible changes to Orchestraitor are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html) once it begins tagging releases.

> **Scope:** release-note entries serve consumers of Orchestraitor (people who build, run,
> or integrate `orc` / `orcd` / the crates). Internal development notes — spec-section
> bookkeeping, repository tooling, agent workflows, review process, issue/task tracking —
> live in pull-request descriptions, spec documents, and evidence files, never here.

## [Unreleased]

### Added

- **Declared tools**: tools are now configuration. Each
  `[tools.<id>]` entry in the layered config declares a tool as either a
  `command` (a fixed argv dispatched through the same Arbitraitor-mediated
  bash seam as the built-in `bash` tool — argv is quoted by Orchestraitor,
  never interpreted from config text) or a `subagent` (a cheap-model
  sub-session with a typed internal-tool allowlist, operator-authored
  instructions, and per-tool budgets). Sub-sessions cannot spawn
  sub-sessions and cannot deliver. Tool definitions are honored from
  trusted config layers only (built-in defaults, plugin defaults, user,
  org); a definition in the project layer is a typed startup error naming
  the tool — never a silent drop. Every declared-tool call is receipted
  with static reason codes; sub-session output is size-capped,
  marker-wrapped untrusted data. Per-tool settings: `effort =
  low|medium|high` (the sub-session's model calls request that reasoning
  tier), `max_summary_bytes` (the byte cap on the finish summary returned
  to the parent, falling back to `budget.max_result_bytes`), and
  `structured_summary` (the finish summary must be a compact fielded
  payload, not prose). An unknown effort value, or a zero `max_turns`,
  `max_result_bytes`, `wall_clock_secs`, or `max_summary_bytes`, is a
  typed startup error. Built-in declarations ship for `explore` (read-only
  explorer, structured summaries) and `review` (the declarative surface
  the pre-landing review loop consumes), both visible to the `implement`
  role. A sub-session runs inside the parent's remaining wall clock, its
  spend lands in the same cost ledger under a per-invocation session id,
  and it may dispatch only the internal tools its allowlist names
  (read-only by default).
- Decision-provider support for the **System One decision protocol** (spec
  §9.45) — an open protocol (single-shot typed questions, calibrated
  probabilities, zero generated text) served by multiple endpoints, not a
  vendor feature: a new protocol-level `SystemOneDecisionProvider` in
  `orchestraitor-provider-api` implements the `DecisionProvider` trait
  against any System One-compatible endpoint through the shared transport
  stack (no new dependency). The reference model is Clef Flash (Cloudflare,
  open source, Apache-2.0), the default on the Neuralwatt cloud deployment.
  Configure it with the layered config key `routing.provider = "systemone"`
  plus the REQUIRED `routing.base_url` (the Neuralwatt cloud
  `https://api.neuralwatt.com/v1` or any self-hosted decision engine) —
  still **default off**; a project without a `[routing]` block keeps the
  heuristic table as the router and no network calls are made. The
  configuration is per-project: each project picks its own endpoint, model
  (`routing.model`, default `clef-flash`), and credential
  (`routing.api_key`, absent or `none` = no auth) — or none.
  When configured, `orc routing resolve` consults it for role resolution and
  `orc campaign run` / `orc loop` consult it for campaign task selection
  before the deterministic P0-first selector; any provider error, a proposal
  outside the eligible set, or an unresolvable `routing.api_key` credential
  engages the documented fallback (a key failure is a typed startup error, not a silent
  degrade) and the cause is recorded in the decision record
  (`precedence_path` / `rationale`). The credential never enters an
  error or log line. The trait also gains two typed decision surfaces for
  follow-up slices — task splitting (`propose_task_split`) and tool selection
  (`propose_tool_selection`) — implemented as default methods returning a
  typed `Unsupported` error, so adding them breaks no existing implementation.
- The rule-driven pre-landing simplify pass, surfaced as
  [`orc simplify run`](docs/cli/orc-simplify.md) and wired into lefthook
  (`pre-commit` `simplify` + `pre-push` `simplify-push`): runs the tools the
  repository already uses (cargo clippy, cargo fmt, rumdl, cargo-machete)
  over a worktree and reports typed suggestions in the three §9.5
  normalization classes — Format (auto-applies under
  `--fix format` + `simplify.auto_apply_format`, the default), Safe fix
  (suggest-only by default; auto-applies only under `--fix safe` +
  `simplify.auto_apply_safe_fixes = true`, scoped to clippy
  machine-applicable suggestions on `.rs` files pending the Arbitraitor
  output-classification gate), and Semantic (suggest-only, never
  auto-rewritten). Clippy checks and fixes both use `--all-targets`; a
  safe-fix suggestion is marked applied only when a verify check after the
  fix no longer reports it. Fail-open by design: an absent tool or a
  timeout is a typed `ORC-SIMPLIFY-001` warning, never a blocker — the only
  non-zero exit is `--pedantic-check` (the pre-push hook's fast-feedback
  signal; the pre-landing enforcement point is the push path, landing with
  the review-loop PR). New layered config table `[simplify]` with built-in
  defaults: `enabled = true`, `auto_apply_format = true`,
  `auto_apply_safe_fixes = false`, `max_passes = 2`, `max_files = 200`,
  `pedantic = false`, and `model_pass = false` (parsed but not wired yet —
  it activates with the review-loop PR's shared sub-session runtime).
||||||| f21d7fb
- SQLite-backed audit store (`SqliteAuditStore`) in `orchestraitor-events` for
  durable §9.17 audit persistence. The store performs hash-chain validation
  that detects inconsistencies between envelope bytes, hashes, and metadata —
  it does not protect against a database-level rewriter, who can delete or
  rewrite rows and recompute the unkeyed SHA-256 chain consistently:
  file-backed (`open`) stores with WAL journaling and in-memory
  (`open_in_memory`) stores without it. Canonical envelope bytes are the sole
  record of truth (the `hash`, `category`, `schema_version`, and
  `monotonic_seq` columns are untrusted index metadata recomputed and
  cross-checked on every read — tampering with a stored blob or column is
  quarantined as a typed `RecordHashMismatch`/parity error; malformed
  `envelope_json` bytes are likewise rejected on read, though the failure
  surfaces as a JSON decoding error rather than those typed variants), full
  hash-chain validation over the decoded set on read, and append-time
  validation identical to the in-memory store (records failing validation are
  refused and leave the store untouched, so an interrupted append can never
  persist a broken chain). Serialized append and import under an immediate
  write transaction so concurrent writers cannot interleave a history
  replacement and persist a chain broken at the seam.
- `orc github` service-identity enforcement is now configurable through the
  layered config key `github_app.enforcement` (`recommended` | `required`,
  default `recommended`). In `recommended` mode, when the `github_app` config
  is absent or partial, mutating GitHub operations from the skill-script
  wrapper take the labelled personal-auth fallback with a loud WARNING;
  direct `orc github` subcommands fail closed when complete configuration is
  unavailable, as do configuration-resolution errors and failed
  enforcement-config reads. In `required` mode the fail-closed behavior
  applies everywhere and `orc github gh-env` additionally pins the bot
  identity (`GIT_AUTHOR_*`/`GIT_COMMITTER_*`) into the child environment, so
  a delegated child cannot stamp personal attribution even via
  `git -c user.email=… commit` (the env vars beat per-invocation config):
  the command refuses to delegate with a typed error naming the missing
  `github_app.*` keys (no personal fallback), the skill-script wrapper
  returns its typed config error without reaching `gh`, and delegation is
  refused when the repo git identity (`git config user.email`) is not the
  service-identity bot's canonical noreply email
  (`<bot-id>+<slug>[bot]@users.noreply.github.com`,
  resolved live from the App identity — a generic noreply address or a
  suffix look-alike does not pass) — so agent-driven `git commit` paths
  cannot stamp personal attribution onto commits. Wrapper-only deployments
  may pin the mode with `ORC_GITHUB_APP_ENFORCEMENT=required`; a declared
  `required` fails closed even when the `orc` binary is unavailable, and
  `orc github gh-env` honors the same pin inside orc: an env-pinned
  `required` runs the full required-mode gate (complete-config check plus
  the canonical git-identity check) even when the layered config does not
  declare it; invalid pin values fail closed. Invalid config values fail
  closed at parse time. The default is a documented bootstrap
  deviation (spec `10-orchestrator.md` §9.41): enforcement must be
  `required` at public release.
- The `decision.record` coordinator decision tool: persists one
  append-only, replayable decision record (kind, selected task, role,
  model+provider, worker arguments, rationale, alternatives considered) into
  the campaign store — the same record shape and store `orc campaign run
  --once` writes, never a second format. Append-only: no update or delete
  path exists on the tool; a re-append creates a new row and the original
  row is never touched. Malformed records (missing required fields — which
  fail deserialization as MCP `invalid_params` errors before the tool runs,
  kind/reason inconsistencies, field-bound overflows) are refused with typed
  reasons, leaving the store and audit log untouched. Records carrying
  secret-shaped material (`secret://` URIs, `sk-`-prefixed keys, Bearer
  tokens, GitHub tokens, long hex/base64 runs) are REFUSED fail-closed —
  never silently redacted (see the tool docs for ownership of payload
  classification). Instruction-shaped content is inert data: stored and
  replayed verbatim, never executed. Every successful append records an
  audit event with the delegation chain (`chain_source: client-asserted`,
  `claimed:`-prefixed labels); the audit event carries only a summary,
  never record content. The MCP tool requires a session-scoped in-memory
  store (append-only row ids observable across the session); without one the
  gateway disables the tool and calls are refused with
  `decision_record_unconfigured`. Documented in
  [docs/cli/orc-decision-record.md](docs/cli/orc-decision-record.md).
- The `board.move` coordinator decision tool: guarded board status-class
  transitions —
  never a raw provider write. Transitions validate against the
  workflow-policy matrix (scheduling forward, pause/resume, completion,
  retirement; `Triage` is a human/PM gate both ways, reopening `Done` and
  unclassified columns refuse), enforce the §9.40 unresolved-blocker rule
  (an item with unresolved `blockedBy` edges cannot enter In Progress), and
  lease-check against a session lease registry (§9.24.2) whose check-and-claim is
  ONE atomic operation (concurrent sessions can never both move an unleased item);
  `In Progress` and the held states (`Blocked`, `Approval Required`, `Input
  Required`) are lease-protected — pausing keeps the lease. Another session's live
  lease refuses `lease-conflict` naming the holder; the session's own expired lease
  refuses `lease-expired`. An applied transition writes through the provider and is
  verified by read-back — reconcile-visible, board-wins on the next tick (§9.43); a
  landed write whose outcome cannot be verified (read-back failure or concurrent
  drift) reports a typed `indeterminate` outcome (board state unknown — re-read
  before retrying), never a false refusal; a landed write whose lease bookkeeping
  fails reports applied plus a typed warning. Every invocation — applied, refused,
  or indeterminate — records to the event store as a `ToolRequest` event with the
  §9.25.1 delegation chain (`chain_source: client-asserted`, `claimed:`-prefixed
  labels; the gateway tool accepts `delegation_chain` on the request), same
  mechanism as `board.query`; an unrecordable invocation fails closed with the
  event-store gap visible. Refusals
  are typed outcomes with static log-safe reason classes; the board is
  unchanged after any refusal, and the CLI renders refusals as decisions
  (exit 0). Scope is status-class transitions only:
  requested field or edge writes refuse `out-of-scope`, never silently
  narrowed. Request values are untrusted input (§6.1) — a hostile status
  name matches nothing and refuses `unknown-status`. Surfaced as
  `orc board guarded-move` and as the `board.move` MCP gateway tool when a
  board provider AND lease registry are configured; when either is missing,
  the gateway's tool router disables both board tool routes. Documented in
  [docs/cli/orc-board.md](docs/cli/orc-board.md).
- The `board.query` coordinator decision tool: a read-only, typed query over
  a `BoardProvider` with two modes — a conjunctive filter search (item type, status, typed
  field values) returning typed items (id, type, title, status, fields,
  dependency edges), never raw provider JSON; and a blocked-graph mode that
  walks the board's native `blockedBy` edges transitively with back-edge
  cycle detection (a diamond DAG is not a cycle; a genuine cycle is surfaced
  as a typed board-data-corruption indication per §9.40, never looped).
  Every invocation is recorded in the event store as a `ToolRequest` event
  carrying the tool name, the filters as data (string values truncated), a
  result summary, and the §9.25.1 delegation chain labeled
  `chain_source: client-asserted` with a `claimed:` prefix per label — the
  tool never fabricates verified identity. The event store is per-invocation
  (written and hash-chain-validated, not yet durable). Filter values are
  untrusted input: instruction-shaped values are matched as inert
  data and never executed. Surfaced as `orc board query` and as the
  `board.query` MCP gateway tool when a
  board provider is configured; when none is, the gateway's tool router
  disables the route (hidden from `tools/list`, calls rejected). Documented
  in [docs/cli/orc-board.md](docs/cli/orc-board.md).
- Owner quickstart for the `orc loop` bootstrap runner in the mdBook
  ([Bootstrap Loop Quickstart](book/src/getting-started/loop-quickstart.md)): prerequisites
  (board config, explicit board auth, optional GitHub App service identity), first
  invocation, what a normal pass does, how to read `loop.db` rows and the end-of-run
  summary, stall/timeout/budget-stop semantics, safe shutdown, and the fixed bootstrap
  guard set. Served on GitHub Pages with the rest of `book/`. Documented at
  [book/src/getting-started/loop-quickstart.md](book/src/getting-started/loop-quickstart.md)
  and linked from the README.
- GitHub Pages documentation site (the mdBook under `book/` builds and deploys
  via the previously disabled `docs.yml` workflow): the book introduction now
  reflects the loop-first product framing and links to the rendered repo docs
  instead of broken out-of-tree paths.
- `orc github push-branch`: lands a local branch's tree as ONE App-signed
  squashed commit on the remote branch via GraphQL `createCommitOnBranch` —
  the agent push path for repositories with `required_signatures` rulesets,
  where plain `git push` produces unverified commits that are rejected at
  merge time. The commit is created on a temporary branch
  (`push-branch/tmp-<tree>`, bootstrapped via `createRef` — the mutation does
  not auto-create branches) at the observed remote head (or the base commit
  for a new branch) so the tree-equality and `signature.isValid` gates run
  before the real ref moves; the swing is `updateRefs` with
  `RefUpdate.beforeOid` set to the observed remote head (exact-head
  precondition, `force = false`): a concurrent writer advance OR rewind of
  the branch makes the precondition fail with a typed error instead of being
  overwritten. The temp ref is invocation-unique (uuid
  name) and deleted on every path. An empty diff (the local tree equals the
  remote head tree — the head is read before the decision) is a no-op. Gates
  fail closed with typed errors (no `git push` fallback); a failed gate
  deletes the temp ref and leaves the real branch untouched. The
  `github-pr-lifecycle` skill's push steps now route through this subcommand
  instead of `git push`.
- `orc github verify-identity [path]`: verifies the ambient git commit
  identity of a checkout (`git config user.name`/`user.email`) against the
  App-derived canonical commit identity (`commit-author` derivation, never
  hardcoded) and exits with a typed error carrying both exact fixes —
  `git -c user.name=… -c user.email=… commit …` and the equivalent
  `git config` pair — on mismatch. An unset identity is also a typed error
  (git would fall back to an auto-detected identity). This is the Rust twin
  of the `commit-identity` skill script: the shell script stays for agents'
  pre-commit use; `orc github verify-identity` now provides the same gate
  natively for the incident class where a fresh worktree inherits the global
  personal gitconfig and stamps the human owner's identity onto PR commits
  (run it in CI or pre-commit hooks to enforce the check on those paths).
- `orc github push-branch` now works out of the box from a worktree: the
  target repository resolves from the `origin` remote (with `--owner`/`--repo`
  as typed overrides; `--repo` accepts either a bare name or the full
  `owner/name` slug, which is used verbatim rather than re-prepended with the
  origin-derived owner), the local branch defaults to HEAD, and the commit
  message body comes from `--body-file` (`-` = stdin). A `--dry-run` flag
  prints the resolved landing plan (paths and statuses only, never blob
  contents) without touching the remote. The diff base for a NEW remote
  branch defaults to the repository's default branch read from the API
  (previously only `origin/main`/`main` resolved locally); for an EXISTING
  remote branch the diff base stays that branch's remote head tree. The
  `updateRefs` false-negative is fixed: when the ref-swing mutation reports a
  GraphQL error or a missing `clientMutationId` but a re-read proves the
  branch moved to the new head, the landing is reported as a success with a
  WARNING on stderr instead of a spurious typed error. Added files that are
  executable (100755) locally trigger a stderr WARNING that
  `createCommitOnBranch` `FileAddition` always lands 100644 (the limitation
  is documented; the tree gate still refuses a mode-mismatched landing).
  Multiple local commits continue to squash-land as ONE App-signed commit
  (documented).
- Per-agent cost tracking for loop-run workers: `orc loop` now opens a durable cost ledger at
  `.orchestraitor/cost.db` and records one cost entry per worker model call,
  attributed to the board task (agent), the routed orchestration role, and
  the loop invocation + task session. The `--json` end-of-run summary gains
  an `agent_costs` array (per-agent token totals and request counts); text
  output gains an `agent costs:` section with the same rollups. A ledger-open failure disables attribution for the run (stderr
  warning; delivery continues). A summary-query failure keeps recorded
  attribution intact but omits cost rows from the end-of-run summary.
  Cost bookkeeping never blocks delivery either way.
- `orc loop` anti-stuck guardrails (default-on; spec `10-orchestrator.md`
  §9.27.1/§9.36, 50-contracts-data.md §21.10). Worker-side:
  `FailureClass::ToolLoopChurn` (the same normalized tool-call shape
  repeating 4 times within an 8-turn window kills the attempt — the
  mktemp-loop failure mode), `FailureClass::NoProgress` (5 consecutive
  identical worktree progress fingerprints fail the attempt), and
  `FailureClass::PollBudgetExhausted` (cumulative poll-shaped bash —
  `sleep` + `gh pr checks`/`gh run watch` fingerprints — exceeding 30m per
  attempt; the task parks blocked-on-external instead of burning the
  session). Churn and no-progress kills are re-plan-fatal: a fresh re-plan
  note cannot fix a loop the model is mechanistically stuck in. The
  wall-clock stall detection is unchanged. Loop-side: a durable per-task
  retry budget in `loop.db` (`task_retry_state`: total attempts, last
  failure class, no-progress streak, backoff window) — a task that exceeds
  `max_task_attempts` (3) is excluded from selection with a typed skip
  reason on the pass decision record and never silently re-selected, and a
  failed task is excluded for its `task_retry_backoff` (15m) window;
  completed runs clear the backoff and streak. The `orc loop` summary
  carries `tasks_stuck` and `task_budget_skips` counters. All thresholds
  are configurable via the layered config keys
  `loop.no_progress_turns`, `loop.tool_repeat_count`,
  `loop.tool_repeat_window`, `loop.ci_poll_budget_secs`,
  `loop.max_task_attempts`, `loop.task_retry_backoff_secs` (absent block =
  defaults active; an explicit `0` disables a guard deliberately, reported
  as a stderr warning — never silent).

### Changed

  A `--re-land` flag re-signs a rebased PR branch whose tree already matches the
  remote head: it lands one empty App-signed commit on top of the remote head so
  the evaluated head chain is fully verified (fixing the unsigned-head case that
  forced plain pushes after conflict rebases); it is a no-op on an already
  verified head and fails closed if the landing is not verified.

- **Service-identity enforcement is now pinned to `required` for this
  repository** (owner mandate, 2026-10-02: no GitHub writes under the
  personal account, ever). `github_app.enforcement = "required"` in
  `orchestraitor.toml` makes every mutating GitHub operation fail closed with
  a typed config error when the `github_app` block cannot resolve — the
  labelled personal-auth fallback no longer applies on this repo.
- **PR creation, PR comments, and review posts now require the
  service-identity wrapper** (skill-script behavior): the pr-lifecycle skill
  ships `pr-create`, `pr-comment`, and `pr-review-post`, which route `gh pr
  create`, `gh pr comment`, and `gh pr review` through `orc_lib_gh_service`
  (`orc github gh-env --`) so writes attribute to the `arbsec-agent` App
  installation, never a personal account. If the service path fails — missing
  or partial `github_app` config, minting failure, `orc` unavailable — the
  operation fails with its typed error and must be reported to the
  orchestrator; falling back to personal auth is forbidden. Workers must run
  from a fresh checkout (a stale checkout predating the wrappers silently
  bypasses them) with `orc` on `PATH` (`ORC_BIN` overrides the binary name).
  `pr-review-post` never approves: `--approve` is refused with a typed error
  (approvals require an independent authorized reviewer; the wrapper's
  service identity authors the PRs it reviews, and GitHub forbids
  self-approval) — `--comment` and `--request-changes` are supported.
  Flag arguments are validated (a trailing `--title`, `--body`, or
  `--body-file` without a value is a typed argument error, exit 2), and
  `--body-file` posts the file contents rather than the file path. The
  `required` enforcement pin is honored even when the `orc` binary is
  unavailable or the working tree cannot be resolved: the operation fails
  closed with its typed config error instead of the personal fallback.
- The review-thread reply path is fail-closed before the write: `pr-thread-reply --resolve` verifies the supplied thread belongs to the PR and contains the replied-to comment BEFORE the reply is posted, so a wrong thread ID is a typed refusal with no reply written (a retry cannot double-post).
- The conflict gate's `pr view` precondition read reports a read FAILURE as its own typed refusal (fail-closed immediately) instead of masquerading as a "GitHub still computing" UNKNOWN that retries twice.
- `review-thread-reply` reference examples route through the service identity (`orc github gh-env --`); the `addPullRequestReviewThreadReply` example no longer selects the unsupported `thread` field that fails GraphQL validation.
- README updated to lead with the bounded, self-improving delivery loop as the
  product's first axis (spec `00-overview.md` §1, §2.3, §3.1), with a new
  "The delivery loop" section describing the stage cycle, the guard set, and
  durable run state, and a "Now running" subsection listing the surfaces that
  ship today.
- GitHub App service-identity write path (#445): `orc github api` executes one
  authenticated GitHub REST call as the App installation (`gh api`-shaped
  `--input`/`--field` body — `-f` values are always strings and GET/DELETE
  fields ride the URL query string — verbatim body on stdout, exit code from
  the HTTP status class), `orc github gh-env -- <command…>` runs one child with
  `GH_TOKEN` set to a freshly minted installation token (token never printed by
  `orc`; child exit code propagates), and `orc github commit-author` prints the
  App's canonical commit identity (`name=`/`email=`) derived from `GET /app`,
  not hardcoded. Every diagnostic stays token-free: the installation token is
  held in memory only, injected solely into the `Authorization` header or the
  child environment, and never printed, logged, or persisted. Removing the
  `github_app` configuration fails all four `orc github` subcommands closed.
- Pluggable decision-model selection behind the `DecisionProvider` trait
  (spec `30-model-routing.md` §9.45; #329): decision providers return typed
  structured outputs with calibrated confidence (`0.0..=1.0`, validated at
  the boundary — NaN and out-of-range values are rejected, never clamped)
  plus the alternatives they considered with per-alternative skip reasons.
  The trait lives in `orchestraitor-provider-api` as a new single-shot
  structured-output provider class — no message streams, no chat surface.
  The deterministic table-driven `FixtureDecisionProvider` ships as the
  offline conformance target. Router integration is **default off**
  behind the new `routing.provider` config key: unset keeps the heuristic
  table as the router (byte-identical behavior), `routing.provider =
  "fixture"` consults the provider first, and any other value is a typed
  unknown-provider error. When a configured provider errors or proposes
  something unroutable, the heuristic table fallback engages and the
  unavailability is documented in the decision record's `fallback_reason`;
  provider-backed resolutions record the provider and confidence in
  `precedence_path`. Documented in
  [docs/cli/orc-routing.md](docs/cli/orc-routing.md).
- Custom orchestration roles via layered configuration: the role registry is
  now configuration, not a hardcoded taxonomy (spec `30-model-routing.md`
  §9.45, §9.22.4; #326). Any `roles.<id>` key defined in a configuration
  layer registers a custom role that `orc routing resolve --role <id>` and
  `orc config get roles.<id>.routing.*` resolve through the same path as the
  six built-in roles — identical layer precedence, field-wise merge, typed
  errors for partial entries, and the `neuralwatt` bootstrap fallback
  (`glm-5.3-flash`). Role ids follow the provider id shape (1–64 lowercase ASCII
  letters, digits, `-` or `_`); ids that are neither built-in nor configured
  remain a typed `unknown role` error. Same-layer shard conflicts on
  `roles.<id>.routing.*` keys are rejected by `orc config validate` with the
  ambiguous-conflict error naming both sources.
- `orchestraitor-board-contract` crate: the pluggable `BoardProvider` contract
  (spec `10-orchestrator.md` §9.43, #318) covering all six contract areas —
  items (stable `BoardItemId` identity, type, title, opaque body), statuses,
  typed custom fields, native `blockedBy` dependency edges, cross-references,
  and typed conjunctive search — with a typed error per failure class. Ships a
  deterministic in-memory reference provider (the conformance target for all
  future providers) and a write-through read-cache (`CachedBoard`) implementing
  the same trait per Ledger D5: single canonical provider, writes reach the
  provider before the cache, cached reads carry a `last_synced` stamp, and a
  board-wins reconcile emits a typed `board-diverged` event whenever provider
  state diverges from a write's assumption. A reusable conformance suite
  ships as the crate's default-on `conformance` cargo feature.
- `orc campaign run --once [--json]` and the `orchestraitor-campaign` crate: the one-shot
  campaign pass (spec `10-orchestrator.md` §9.35, `60-milestones.md` M1; #313). Each
  pass reads the reconciled board through the #308 provider, applies the minimal
  epic-focus rule (items whose configured priority field carries `P0` first, stable
  `(repo, issue-number)` order), selects
  at most one eligible task, persists exactly one append-only decision record to the
  local SQLite store (`<config-dir>/campaign.db`, `schema_migrations`-versioned), and —
  for a selection — spawns the worker via the daemon-less direct path (deterministic
  repo-scoped `board-<owner>_<repo>-<number>` task id; charset-violating or
  over-long repo names carry an 8-hex digest suffix to stay collision-resistant).
  No-op passes
  persist typed reasons
  (`empty-queue`, `all-blocked` with the blocked graph attached, `epic-exhausted`) and
  spawn nothing; unevaluable board items (fail-closed reads) are disclosed on every
  record.
  Documented in [docs/cli/orc-campaign.md](docs/cli/orc-campaign.md) and the README.
- `orc loop [--json] [--max-cycles N]` and the loop runner in `orchestraitor-campaign`:
  the cron-shaped foreground bootstrap loop (spec `10-orchestrator.md` §9.36 thin slice;
  #314). Each cycle polls the board, plans one campaign pass (one append-only decision
  record), spawns the worker on the daemon-less direct path, and supervises in-flight
  runs under the issue-#310 guard set shared with the worker: concurrency cap 2,
  supervisor-side stall kill (beat staleness over the 10m window — catches runs wedged
  inside hung transport calls), worker-timeout kill at 45m, pass pacing at 10s·2^n
  capped 5m, $10/day spend soft cap (seals the intake and drains in-flight work — never
  aborts; inert by default — the per-token spend estimate is `0.0` until the
  cost-ledger lane wires provider pricing in, so no spend accrues and the cap is never
  reached — matching [docs/cli/orc-loop.md](docs/cli/orc-loop.md)), and a 4h whole-run
  budget. SIGTERM/SIGINT drain gracefully inside the 5s
  daemon budget and record stragglers; a second signal short-circuits the grace. A
  second concurrent invocation is rejected (`loop-already-running`, advisory flock).
  A task that cannot be started (missing task fixture, unsupported provider,
  transport build failure) is a per-task outcome, not a run-killer: the task is
  recorded as a terminal `failed` run row, the pass is paced out, and the loop keeps
  supervising in-flight work — the failed task is excluded for the rest of the
  invocation. A fatal durable-state failure aborts and records in-flight workers
  before the run returns, so no row is stranded `running`. The 4h run budget bounds
  the board poll too — the budget cannot expire while the loop sits inside a board
  request, and nothing is planned or spawned after expiry; transient poll failures
  back off and retry (counted in the summary, never fatal). Each concurrent worker
  slot gets its own git worktree under `<config-dir>/loop-worktrees/` keyed by task
  id — concurrent workers never share or write the project directory. One worker run
  per task per invocation — failed or killed tasks are never silently
  retried; retry is a fresh board-driven selection in a later invocation. Run state is
  recorded at `<config-dir>/loop.db` for guard accounting and run outcomes. Documented
  in [docs/cli/orc-loop.md](docs/cli/orc-loop.md) and the README.
- `orc worker run --task <id> [--json]` and the `orchestraitor-worker` crate: the headless
  one-shot bootstrap mini-worker (spec `10-orchestrator.md` §9.38, `60-milestones.md` MVP-6;
  #310). The worker resolves a fixture task, routes through the control plane's `implement`
  role decision, and runs a bounded mini-agent loop over the `ProviderTransport` boundary
  with exactly four tools — worktree-confined file read and write (symlink-escape-safe,
  size-capped; writes recorded as untrusted output per spec §9.14), minimal local content
  search, and bash that crosses the Arbitraitor #311 mediation boundary
  (`MediatedWorker::spawn` preflight + `run_bash`) on every call. Capability requests
  outside the four tools are refused, recorded, and fed back; every tool call produces a
  receipt with static reason codes (spec §9.23.4). Budgets are enforced as typed
  failures, never infinite retries: attempts 3, re-plan 2, worker timeout 45m,
  concurrency 2 (scheduler-facing), stall 10m, provider backoff 10s·2^n capped at 5m,
  $10/day spend soft cap (recorded, not a hard stop), run budget 4h. Delivery is a seam
  (`DeliverySink`): the bootstrap reports `delivery.kind = "pending"` and the PR delivery
  path wires in a later lane. The worker loop is tested against the deterministic
  simulator, never a live provider (spec §21.3). Documented in
  [docs/cli/orc-worker.md](docs/cli/orc-worker.md) and the README.
- `orchestraitor-provider-neuralwatt` crate implementing `ProviderTransport` against the
  Neuralwatt OpenAI Chat Completions-compatible API for GLM-5.2 BYOK (spec §10.3). Default
  base URL `https://api.neuralwatt.com/v1` (overridable via config); API key resolved from
  `secret://keyring/neuralwatt` or `NEURALWATT_API_KEY`. Streaming via SSE into `ModelEvent`
  values; per-call cost entries per spec §9.19.4. The legacy Zhipu endpoint `open.bigmodel.cn`
  is rejected at configuration time (spec §10.3).
- `orchestraitor-provider-proxy` crate with OpenAI Chat Completions, OpenAI Responses,
  Anthropic Messages, `/v1/models`, short-lived local tokens, upstream BYOK credential
  isolation for child processes, per-completion cost attribution, and explicit Mode D
  trust-boundary reporting per spec §10.1.
- Durable daemon state in `orchestraitor-daemon`: SQLite WAL metadata store with schema
  migrations, hash-chained event persistence, and a content-addressed filesystem store for
  artifacts (spec §9.17).
- Baseline harness indexing in `orchestraitor-context`: content-addressed tree-sitter indexer
  with provenance envelopes on every emitted context item (spec §9.15.1); the Appendix E
  context query API ships as MVP stubs pending §9.16 LSP-backed wiring.
- Repository governance and community documents: contribution guidance, security policy, code
  of conduct, and support documents, adapted from the sibling Arbitraitor repository.
- Dual `MIT OR Apache-2.0` licensing, matching Arbitraitor.
- Bootstrap-worker sandbox mediation in the `orchestraitor-arbitraitor-client`
  crate (spec `40-arbitraitor-integration.md` §9.6, §6.7):
  `MediatedWorker::spawn` probes
  `arbitraitor_sandbox::compute_effective_controls(SandboxMode::Restricted, platform)`
  as a capability preflight, records the controls matrix + verdict into
  `WorkerPreflight` for the run state, and fails closed — typed
  `MediationError::UnavailableControls` naming each missing control, typed
  `UnsupportedPlatform` on non-Linux (ADR-0024; no non-secure bootstrap
  mode) — before any execution surface exists. `MediatedWorker::run_bash`
  routes scripts through `arbitraitor_exec`'s mediated bash
  (`ExecutionContextBuilder` via `ScriptExecution`) under an explicit
  network-denied `ExecutionPolicy`; errors translate Arbitraitor `ExecError`
  into log-safe static reason codes (no command output/args leakage, spec
  §9.23.4). Documented in `docs/sandbox-mediation.md` and in the README.
- GitHub App service identity (`arbsec-agent`) token-minting path (spec
  `10-orchestrator.md` §9.25.2, §9.41; issue #307). Layered config gains
  `github_app.slug` (built-in default `arbsec-agent`), `github_app.client_id`,
  `github_app.installation_id`, `github_app.private_key_uri` (`secret://` URI,
  fail-closed resolution — no ambient-credential fallback), and top-level
  `service_identities` (default `["arbsec-agent"]`). `orchestraitor-core`
  gains `GitHubAppAuth`: RS256 JWT minting with `iss = <client_id>` (GitHub
  rejects the numeric app ID with 401) and `exp = iat + 10min`, an
  installation-token cache re-minting at `expiry − 5min` behind a
  single-flight mutex+condvar so concurrent requests never duplicate mints,
  response-contract validation (1h lifetime cap, parsable RFC 3339 expiry),
  and redacted `Debug`/error output (no PEM/JWT/token material, spec
  `40-arbitraitor-integration.md` §9.23.4). `secret.rs` gains exact-store
  `SecretUri::resolve` (env + OS keyring behind the default `secrets-keyring`
  feature). New CLI subcommand `orc github mint-token` prints only non-secret
  metadata (installation id, expiry, SHA-256 fingerprint prefix). Documented
  in `docs/cli/orc-github.md` and README (#307).
- Documented the hidden test/debug override `ORCHESTRAITOR_GITHUB_API_ENDPOINT`
  (`--github-api-endpoint`) for `orc github mint-token`, including the security
  note that it redirects where the App JWT bearer is sent
  (`docs/cli/orc-github.md`) (#307).
- Static heuristic role routing for the six built-in orchestration roles (`explore`,
  `research`, `plan`, `implement`, `review`, `verify`) per spec `30-model-routing.md`
  §9.45 and spec §9.19.2: `roles.<id>.routing.*` keys are first-class configuration
  (known-key reporting, layered merge, `orc config get roles.implement.routing.provider`
  resolves out of the box via shipped built-in defaults for the Neuralwatt GLM-5.2
  single-provider bootstrap of spec §10.3). `orc routing resolve --role <id> [--json]`
  resolves a role through the layered configuration and persists a routing decision
  record (role, provider, model, precedence path, fallback reason) to the local
  `SQLite` store (`<config-dir>/routing.db`, WAL, `schema_migrations`-versioned).
  Incomplete effective entries fail closed with a typed error naming the missing
  configuration key; a role with no entry in any layer resolves via the documented
  bootstrap default and records the fallback reason. No decision model, no custom
  roles, no subscription awareness (E2) (#309).
- `orchestraitor-board` crate and `orc board` command: the bootstrap GitHub Projects v2
  board provider (spec `10-orchestrator.md` §9.43, §9.40; #308). `orc board ready [--json]`
  lists OPEN leaf Task/Bug items with `Target=MVP`, `Status=Ready`, and no unresolved
  `blockedBy` edges on the shared board (a closed issue is excluded with a warning even when
  its board fields say Ready/MVP; fail-closed on truncated
  `blockedBy`/field/label windows;
  malformed items are skipped with a warning, never a crash), and `orc board move <issue>
  --status "<Status>"` resolves the item across all configured repositories (ambiguous
  same-numbered issues fail with a typed reference error), writes the Status single-select
  field and verifies the write by
  read-back. Node IDs resolve at runtime from the human-readable names in
  `.agents/project/github-project.local.toml` and are cached under
  `$XDG_CACHE_HOME/orchestraitor/` — never inside the repository. Auth is injected via the
  `BoardAuth` trait; the bootstrap `SecretUriAuth` stub resolves the configured
  `secret://` URI (env-backed) and never sniffs ambient credentials; a hidden
  `--github-graphql-endpoint` override serves GHES instances.

### Changed

- The tree-sitter baseline indexer (`orchestraitor-context`) now builds against
  tree-sitter 0.27.0 (previously 0.26.11) to align with the Arbitraitor 0.27
  migration; the bump pulls in upstream's UTF-16 buffer over-read fix.

### Fixed

- Simplify: an empty staged index drops the staged filter instead of hiding
  every file-scoped finding; timed-out tools have their process group (or
  Windows process tree) terminated and their output readers joined; applied
  suggestion counts survive deduplication; `--pedantic-check` returns a
  typed `PedanticCheckFailed` error to library callers after flushing the
  report (the CLI still exits 1).
- `orc github commit-author` authenticates `GET /app` with a freshly minted
  App JWT instead of an installation token: `GET /app` is an App-level
  endpoint that GitHub answers with 401 for installation tokens, so the
  subcommand previously could never deliver its documented output. The bot
  user id is resolved with a follow-up `GET /users/{slug}[bot]` authenticated
  with the installation token (the `GET /app` payload carries the slug but
  not the bot user; GitHub rejects the App JWT on `GET /users`, and on an
  Enterprise Managed Users organization the bot profile is not publicly
  visible, so an unauthenticated request would answer 404). The JWT is
  secret material under the same guarantees as the token: held in memory
  only, injected solely into the one `Authorization` header, never printed,
  logged, or persisted.
- `orc github gh-env` now rejects a child command that exists but lacks the
  executable bit BEFORE minting a token; previously the token was minted and
  then the spawn was guaranteed to fail.
- Skill-script service-identity routing (`orc_lib_gh_service` in the
  github-project-workflow / github-pr-lifecycle skills) now routes ALL
  `gh` call sites that flow through the mutating scripts — the write
  commands (merge-gate, claim-issue assignment, release-issue,
  create-blocker, create-follow-up, decompose-issue) AND their per-script
  precondition reads (issue view/list before a mutation, the create-blocker
  cross-repo node-id lookups) — through the App installation token; only
  the read-only helper `orc_lib_gh` stays on the ambient `gh` auth. The
  caller identity for assignee-ownership checks is resolved from
  `orc github commit-author` on the service path (`gh api user` is
  user-context only and fails with an installation token); the
  `gh api user` probe remains only on the labelled personal fallback. The
  github_app config probe requires all three keys (`client_id`,
  `installation_id`, `private_key_uri`), distinguishes config-absent
  (labelled personal fallback) from a config resolution error (typed
  failure — never a silent personal-auth fallback), and the gh-env handoff
  no longer passes a stray `command` token into the child argv, which made
  every routed call fail. A failed `orc config get
  github_app.enforcement` read fails closed instead of permitting the
  personal fallback (an unset key still falls back in recommended mode).
- An invalid `ORC_GITHUB_APP_ENFORCEMENT` value in the skill-script wrapper
  (anything other than `recommended` or `required`, e.g. `Required`) now
  fails closed with a typed config error: previously an unrecognized pin
  compared false against `required` and — with the `github_app` config
  absent — silently took the labelled personal fallback, the fail-open
  outcome the pin exists to prevent.
- The skill-script caller-identity resolver
  (`orc_lib_resolve_my_login`) now picks the ambient-vs-service route with
  the SAME `github_app` config probe as the service wrapper instead of the
  `orc` binary's presence: with `orc` installed but the `github_app` config
  absent, mutating operations take the labelled personal fallback while the
  identity resolver previously demanded the bot login (which the
  mis-matched route could not resolve), breaking claim/release on exactly
  the machines the fallback exists for. The config-absent case now resolves
  the personal login via `gh api user`, matching the route the writes take;
  a config-probe resolution error yields an empty identity (typed failure,
  never a wildcard match).
- `orc github api` `-f/--field` now behaves like `gh api -f/--raw-field`:
  the value is ALWAYS sent as a string (previously values that happened to
  parse as JSON — `123`, `null`, `{"x":1}` — were sent as that JSON type,
  corrupting comment bodies that are valid JSON). GET/DELETE requests
  encode `--field` pairs as percent-encoded URL query parameters (the `gh
  api` shape) instead of silently dropping them into a JSON body the
  endpoint ignores, and `--input` on GET/DELETE is a typed error rather
  than a body GitHub never reads.
- `orc routing resolve` no longer dirties `git status` when run at the default store
  path: `.orchestraitor/routing.db` and its WAL sidecars are gitignored. Routing
  decision store errors now include the underlying cause in their `Display`
  output (#309).
- `orc github` service-identity enforcement is now configurable through the
  layered config key `github_app.enforcement` (`recommended` | `required`,
  default `recommended`). In `recommended` mode, when the `github_app` config
  is absent or partial, mutating GitHub operations from the skill-script
  wrapper take the labelled personal-auth fallback with a loud WARNING;
  direct `orc github` subcommands fail closed when complete configuration is
  unavailable, as do configuration-resolution errors and failed
  enforcement-config reads. In `required` mode the fail-closed behavior
  applies everywhere and `orc github gh-env` additionally pins the bot
  identity (`GIT_AUTHOR_*`/`GIT_COMMITTER_*`) into the child environment, so
  a delegated child cannot stamp personal attribution even via
  `git -c user.email=… commit` (the env vars beat per-invocation config):
  the command refuses to delegate with a typed error naming the missing
  `github_app.*` keys (no personal fallback), the skill-script wrapper
  returns its typed config error without reaching `gh`, and delegation is
  refused when the repo git identity (`git config user.email`) is not the
  service-identity bot's canonical noreply email
  (`<bot-id>+<slug>[bot]@users.noreply.github.com`,
  resolved live from the App identity — a generic noreply address or a
  suffix look-alike does not pass) — so agent-driven `git commit` paths
  cannot stamp personal attribution onto commits. Wrapper-only deployments
  may pin the mode with `ORC_GITHUB_APP_ENFORCEMENT=required`; a declared
  `required` fails closed even when the `orc` binary is unavailable, and
  `orc github gh-env` honors the same pin inside orc: an env-pinned
  `required` runs the full required-mode gate (complete-config check plus
  the canonical git-identity check) even when the layered config does not
  declare it; invalid pin values fail closed. Invalid config values fail
  closed at parse time. The default is a documented bootstrap
  deviation (spec `10-orchestrator.md` §9.41): enforcement must be
  `required` at public release.
- The `decision.record` coordinator decision tool: persists one
  append-only, replayable decision record (kind, selected task, role,
  model+provider, worker arguments, rationale, alternatives considered) into
  the campaign store — the same record shape and store `orc campaign run
  --once` writes, never a second format. Append-only: no update or delete
  path exists on the tool; a re-append creates a new row and the original
  row is never touched. Malformed records (missing required fields — which
  fail deserialization as MCP `invalid_params` errors before the tool runs,
  kind/reason inconsistencies, field-bound overflows) are refused with typed
  reasons, leaving the store and audit log untouched. Records carrying
  secret-shaped material (`secret://` URIs, `sk-`-prefixed keys, Bearer
  tokens, GitHub tokens, long hex/base64 runs) are REFUSED fail-closed —
  never silently redacted (see the tool docs for ownership of payload
  classification). Instruction-shaped content is inert data: stored and
  replayed verbatim, never executed. Every successful append records an
  audit event with the delegation chain (`chain_source: client-asserted`,
  `claimed:`-prefixed labels); the audit event carries only a summary,
  never record content. The MCP tool requires a session-scoped in-memory
  store (append-only row ids observable across the session); without one the
  gateway disables the tool and calls are refused with
  `decision_record_unconfigured`. Documented in
  [docs/cli/orc-decision-record.md](docs/cli/orc-decision-record.md).
- The `board.move` coordinator decision tool: guarded board status-class
  transitions —
  never a raw provider write. Transitions validate against the
  workflow-policy matrix (scheduling forward, pause/resume, completion,
  retirement; `Triage` is a human/PM gate both ways, reopening `Done` and
  unclassified columns refuse), enforce the §9.40 unresolved-blocker rule
  (an item with unresolved `blockedBy` edges cannot enter In Progress), and
  lease-check against a session lease registry (§9.24.2) whose check-and-claim is
  ONE atomic operation (concurrent sessions can never both move an unleased item);
  `In Progress` and the held states (`Blocked`, `Approval Required`, `Input
  Required`) are lease-protected — pausing keeps the lease. Another session's live
  lease refuses `lease-conflict` naming the holder; the session's own expired lease
  refuses `lease-expired`. An applied transition writes through the provider and is
  verified by read-back — reconcile-visible, board-wins on the next tick (§9.43); a
  landed write whose outcome cannot be verified (read-back failure or concurrent
  drift) reports a typed `indeterminate` outcome (board state unknown — re-read
  before retrying), never a false refusal; a landed write whose lease bookkeeping
  fails reports applied plus a typed warning. Every invocation — applied, refused,
  or indeterminate — records to the event store as a `ToolRequest` event with the
  §9.25.1 delegation chain (`chain_source: client-asserted`, `claimed:`-prefixed
  labels; the gateway tool accepts `delegation_chain` on the request), same
  mechanism as `board.query`; an unrecordable invocation fails closed with the
  event-store gap visible. Refusals
  are typed outcomes with static log-safe reason classes; the board is
  unchanged after any refusal, and the CLI renders refusals as decisions
  (exit 0). Scope is status-class transitions only:
  requested field or edge writes refuse `out-of-scope`, never silently
  narrowed. Request values are untrusted input (§6.1) — a hostile status
  name matches nothing and refuses `unknown-status`. Surfaced as
  `orc board guarded-move` and as the `board.move` MCP gateway tool when a
  board provider AND lease registry are configured; when either is missing,
  the gateway's tool router disables both board tool routes. Documented in
  [docs/cli/orc-board.md](docs/cli/orc-board.md).
- The `board.query` coordinator decision tool: a read-only, typed query over
  a `BoardProvider` with two modes — a conjunctive filter search (item type, status, typed
  field values) returning typed items (id, type, title, status, fields,
  dependency edges), never raw provider JSON; and a blocked-graph mode that
  walks the board's native `blockedBy` edges transitively with back-edge
  cycle detection (a diamond DAG is not a cycle; a genuine cycle is surfaced
  as a typed board-data-corruption indication per §9.40, never looped).
  Every invocation is recorded in the event store as a `ToolRequest` event
  carrying the tool name, the filters as data (string values truncated), a
  result summary, and the §9.25.1 delegation chain labeled
  `chain_source: client-asserted` with a `claimed:` prefix per label — the
  tool never fabricates verified identity. The event store is per-invocation
  (written and hash-chain-validated, not yet durable). Filter values are
  untrusted input: instruction-shaped values are matched as inert
  data and never executed. Surfaced as `orc board query` and as the
  `board.query` MCP gateway tool when a
  board provider is configured; when none is, the gateway's tool router
  disables the route (hidden from `tools/list`, calls rejected). Documented
  in [docs/cli/orc-board.md](docs/cli/orc-board.md).
- Owner quickstart for the `orc loop` bootstrap runner in the mdBook
  ([Bootstrap Loop Quickstart](book/src/getting-started/loop-quickstart.md)): prerequisites
  (board config, explicit board auth, optional GitHub App service identity), first
  invocation, what a normal pass does, how to read `loop.db` rows and the end-of-run
  summary, stall/timeout/budget-stop semantics, safe shutdown, and the fixed bootstrap
  guard set. Served on GitHub Pages with the rest of `book/`. Documented at
  [book/src/getting-started/loop-quickstart.md](book/src/getting-started/loop-quickstart.md)
  and linked from the README.
- GitHub Pages documentation site (the mdBook under `book/` builds and deploys
  via the previously disabled `docs.yml` workflow): the book introduction now
  reflects the loop-first product framing and links to the rendered repo docs
  instead of broken out-of-tree paths.
- `orc github push-branch`: lands a local branch's tree as ONE App-signed
  squashed commit on the remote branch via GraphQL `createCommitOnBranch` —
  the agent push path for repositories with `required_signatures` rulesets,
  where plain `git push` produces unverified commits that are rejected at
  merge time. The commit is created on a temporary branch
  (`push-branch/tmp-<tree>`, bootstrapped via `createRef` — the mutation does
  not auto-create branches) at the observed remote head (or the base commit
  for a new branch) so the tree-equality and `signature.isValid` gates run
  before the real ref moves; the swing is `updateRefs` with
  `RefUpdate.beforeOid` set to the observed remote head (exact-head
  precondition, `force = false`): a concurrent writer advance OR rewind of
  the branch makes the precondition fail with a typed error instead of being
  overwritten. The temp ref is invocation-unique (uuid
  name) and deleted on every path. An empty diff (the local tree equals the
  remote head tree — the head is read before the decision) is a no-op. Gates
  fail closed with typed errors (no `git push` fallback); a failed gate
  deletes the temp ref and leaves the real branch untouched. The
  `github-pr-lifecycle` skill's push steps now route through this subcommand
  instead of `git push`.

### Changed

  A `--re-land` flag re-signs a rebased PR branch whose tree already matches the
  remote head: it lands one empty App-signed commit on top of the remote head so
  the evaluated head chain is fully verified (fixing the unsigned-head case that
  forced plain pushes after conflict rebases); it is a no-op on an already
  verified head and fails closed if the landing is not verified.

- **Service-identity enforcement is now pinned to `required` for this
  repository** (owner mandate, 2026-10-02: no GitHub writes under the
  personal account, ever). `github_app.enforcement = "required"` in
  `orchestraitor.toml` makes every mutating GitHub operation fail closed with
  a typed config error when the `github_app` block cannot resolve — the
  labelled personal-auth fallback no longer applies on this repo.
- **PR creation, PR comments, and review posts now require the
  service-identity wrapper** (skill-script behavior): the pr-lifecycle skill
  ships `pr-create`, `pr-comment`, and `pr-review-post`, which route `gh pr
  create`, `gh pr comment`, and `gh pr review` through `orc_lib_gh_service`
  (`orc github gh-env --`) so writes attribute to the `arbsec-agent` App
  installation, never a personal account. If the service path fails — missing
  or partial `github_app` config, minting failure, `orc` unavailable — the
  operation fails with its typed error and must be reported to the
  orchestrator; falling back to personal auth is forbidden. Workers must run
  from a fresh checkout (a stale checkout predating the wrappers silently
  bypasses them) with `orc` on `PATH` (`ORC_BIN` overrides the binary name).
  `pr-review-post` never approves: `--approve` is refused with a typed error
  (approvals require an independent authorized reviewer; the wrapper's
  service identity authors the PRs it reviews, and GitHub forbids
  self-approval) — `--comment` and `--request-changes` are supported.
  Flag arguments are validated (a trailing `--title`, `--body`, or
  `--body-file` without a value is a typed argument error, exit 2), and
  `--body-file` posts the file contents rather than the file path. The
  `required` enforcement pin is honored even when the `orc` binary is
  unavailable or the working tree cannot be resolved: the operation fails
  closed with its typed config error instead of the personal fallback.
- README updated to lead with the bounded, self-improving delivery loop as the
  product's first axis (spec `00-overview.md` §1, §2.3, §3.1), with a new
  "The delivery loop" section describing the stage cycle, the guard set, and
  durable run state, and a "Now running" subsection listing the surfaces that
  ship today.
- GitHub App service-identity write path (#445): `orc github api` executes one
  authenticated GitHub REST call as the App installation (`gh api`-shaped
  `--input`/`--field` body — `-f` values are always strings and GET/DELETE
  fields ride the URL query string — verbatim body on stdout, exit code from
  the HTTP status class), `orc github gh-env -- <command…>` runs one child with
  `GH_TOKEN` set to a freshly minted installation token (token never printed by
  `orc`; child exit code propagates), and `orc github commit-author` prints the
  App's canonical commit identity (`name=`/`email=`) derived from `GET /app`,
  not hardcoded. Every diagnostic stays token-free: the installation token is
  held in memory only, injected solely into the `Authorization` header or the
  child environment, and never printed, logged, or persisted. Removing the
  `github_app` configuration fails all four `orc github` subcommands closed.
- Pluggable decision-model selection behind the `DecisionProvider` trait
  (spec `30-model-routing.md` §9.45; #329): decision providers return typed
  structured outputs with calibrated confidence (`0.0..=1.0`, validated at
  the boundary — NaN and out-of-range values are rejected, never clamped)
  plus the alternatives they considered with per-alternative skip reasons.
  The trait lives in `orchestraitor-provider-api` as a new single-shot
  structured-output provider class — no message streams, no chat surface.
  The deterministic table-driven `FixtureDecisionProvider` ships as the
  offline conformance target. Router integration is **default off**
  behind the new `routing.provider` config key: unset keeps the heuristic
  table as the router (byte-identical behavior), `routing.provider =
  "fixture"` consults the provider first, and any other value is a typed
  unknown-provider error. When a configured provider errors or proposes
  something unroutable, the heuristic table fallback engages and the
  unavailability is documented in the decision record's `fallback_reason`;
  provider-backed resolutions record the provider and confidence in
  `precedence_path`. Documented in
  [docs/cli/orc-routing.md](docs/cli/orc-routing.md).
- Custom orchestration roles via layered configuration: the role registry is
  now configuration, not a hardcoded taxonomy (spec `30-model-routing.md`
  §9.45, §9.22.4; #326). Any `roles.<id>` key defined in a configuration
  layer registers a custom role that `orc routing resolve --role <id>` and
  `orc config get roles.<id>.routing.*` resolve through the same path as the
  six built-in roles — identical layer precedence, field-wise merge, typed
  errors for partial entries, and the `neuralwatt` bootstrap fallback
  (`glm-5.3-flash`). Role ids follow the provider id shape (1–64 lowercase ASCII
  letters, digits, `-` or `_`); ids that are neither built-in nor configured
  remain a typed `unknown role` error. Same-layer shard conflicts on
  `roles.<id>.routing.*` keys are rejected by `orc config validate` with the
  ambiguous-conflict error naming both sources.
- `orchestraitor-board-contract` crate: the pluggable `BoardProvider` contract
  (spec `10-orchestrator.md` §9.43, #318) covering all six contract areas —
  items (stable `BoardItemId` identity, type, title, opaque body), statuses,
  typed custom fields, native `blockedBy` dependency edges, cross-references,
  and typed conjunctive search — with a typed error per failure class. Ships a
  deterministic in-memory reference provider (the conformance target for all
  future providers) and a write-through read-cache (`CachedBoard`) implementing
  the same trait per Ledger D5: single canonical provider, writes reach the
  provider before the cache, cached reads carry a `last_synced` stamp, and a
  board-wins reconcile emits a typed `board-diverged` event whenever provider
  state diverges from a write's assumption. A reusable conformance suite
  ships as the crate's default-on `conformance` cargo feature.
- `orc campaign run --once [--json]` and the `orchestraitor-campaign` crate: the one-shot
  campaign pass (spec `10-orchestrator.md` §9.35, `60-milestones.md` M1; #313). Each
  pass reads the reconciled board through the #308 provider, applies the minimal
  epic-focus rule (items whose configured priority field carries `P0` first, stable
  `(repo, issue-number)` order), selects
  at most one eligible task, persists exactly one append-only decision record to the
  local SQLite store (`<config-dir>/campaign.db`, `schema_migrations`-versioned), and —
  for a selection — spawns the worker via the daemon-less direct path (deterministic
  repo-scoped `board-<owner>_<repo>-<number>` task id; charset-violating or
  over-long repo names carry an 8-hex digest suffix to stay collision-resistant).
  No-op passes
  persist typed reasons
  (`empty-queue`, `all-blocked` with the blocked graph attached, `epic-exhausted`) and
  spawn nothing; unevaluable board items (fail-closed reads) are disclosed on every
  record.
  Documented in [docs/cli/orc-campaign.md](docs/cli/orc-campaign.md) and the README.
- `orc loop [--json] [--max-cycles N]` and the loop runner in `orchestraitor-campaign`:
  the cron-shaped foreground bootstrap loop (spec `10-orchestrator.md` §9.36 thin slice;
  #314). Each cycle polls the board, plans one campaign pass (one append-only decision
  record), spawns the worker on the daemon-less direct path, and supervises in-flight
  runs under the issue-#310 guard set shared with the worker: concurrency cap 2,
  supervisor-side stall kill (beat staleness over the 10m window — catches runs wedged
  inside hung transport calls), worker-timeout kill at 45m, pass pacing at 10s·2^n
  capped 5m, $10/day spend soft cap (seals the intake and drains in-flight work — never
  aborts; inert by default — the per-token spend estimate is `0.0` until the
  cost-ledger lane wires provider pricing in, so no spend accrues and the cap is never
  reached — matching [docs/cli/orc-loop.md](docs/cli/orc-loop.md)), and a 4h whole-run
  budget. SIGTERM/SIGINT drain gracefully inside the 5s
  daemon budget and record stragglers; a second signal short-circuits the grace. A
  second concurrent invocation is rejected (`loop-already-running`, advisory flock).
  A task that cannot be started (missing task fixture, unsupported provider,
  transport build failure) is a per-task outcome, not a run-killer: the task is
  recorded as a terminal `failed` run row, the pass is paced out, and the loop keeps
  supervising in-flight work — the failed task is excluded for the rest of the
  invocation. A fatal durable-state failure aborts and records in-flight workers
  before the run returns, so no row is stranded `running`. The 4h run budget bounds
  the board poll too — the budget cannot expire while the loop sits inside a board
  request, and nothing is planned or spawned after expiry; transient poll failures
  back off and retry (counted in the summary, never fatal). Each concurrent worker
  slot gets its own git worktree under `<config-dir>/loop-worktrees/` keyed by task
  id — concurrent workers never share or write the project directory. One worker run
  per task per invocation — failed or killed tasks are never silently
  retried; retry is a fresh board-driven selection in a later invocation. Run state is
  recorded at `<config-dir>/loop.db` for guard accounting and run outcomes. Documented
  in [docs/cli/orc-loop.md](docs/cli/orc-loop.md) and the README.
- `orc worker run --task <id> [--json]` and the `orchestraitor-worker` crate: the headless
  one-shot bootstrap mini-worker (spec `10-orchestrator.md` §9.38, `60-milestones.md` MVP-6;
  #310). The worker resolves a fixture task, routes through the control plane's `implement`
  role decision, and runs a bounded mini-agent loop over the `ProviderTransport` boundary
  with exactly four tools — worktree-confined file read and write (symlink-escape-safe,
  size-capped; writes recorded as untrusted output per spec §9.14), minimal local content
  search, and bash that crosses the Arbitraitor #311 mediation boundary
  (`MediatedWorker::spawn` preflight + `run_bash`) on every call. Capability requests
  outside the four tools are refused, recorded, and fed back; every tool call produces a
  receipt with static reason codes (spec §9.23.4). Budgets are enforced as typed
  failures, never infinite retries: attempts 3, re-plan 2, worker timeout 45m,
  concurrency 2 (scheduler-facing), stall 10m, provider backoff 10s·2^n capped at 5m,
  $10/day spend soft cap (recorded, not a hard stop), run budget 4h. Delivery is a seam
  (`DeliverySink`): the bootstrap reports `delivery.kind = "pending"` and the PR delivery
  path wires in a later lane. The worker loop is tested against the deterministic
  simulator, never a live provider (spec §21.3). Documented in
  [docs/cli/orc-worker.md](docs/cli/orc-worker.md) and the README.
- `orchestraitor-provider-neuralwatt` crate implementing `ProviderTransport` against the
  Neuralwatt OpenAI Chat Completions-compatible API for GLM-5.2 BYOK (spec §10.3). Default
  base URL `https://api.neuralwatt.com/v1` (overridable via config); API key resolved from
  `secret://keyring/neuralwatt` or `NEURALWATT_API_KEY`. Streaming via SSE into `ModelEvent`
  values; per-call cost entries per spec §9.19.4. The legacy Zhipu endpoint `open.bigmodel.cn`
  is rejected at configuration time (spec §10.3).
- `orchestraitor-provider-proxy` crate with OpenAI Chat Completions, OpenAI Responses,
  Anthropic Messages, `/v1/models`, short-lived local tokens, upstream BYOK credential
  isolation for child processes, per-completion cost attribution, and explicit Mode D
  trust-boundary reporting per spec §10.1.
- Durable daemon state in `orchestraitor-daemon`: SQLite WAL metadata store with schema
  migrations, hash-chained event persistence, and a content-addressed filesystem store for
  artifacts (spec §9.17).
- Baseline harness indexing in `orchestraitor-context`: content-addressed tree-sitter indexer
  with provenance envelopes on every emitted context item (spec §9.15.1); the Appendix E
  context query API ships as MVP stubs pending §9.16 LSP-backed wiring.
- Repository governance and community documents: contribution guidance, security policy, code
  of conduct, and support documents, adapted from the sibling Arbitraitor repository.
- Dual `MIT OR Apache-2.0` licensing, matching Arbitraitor.
- Bootstrap-worker sandbox mediation in the `orchestraitor-arbitraitor-client`
  crate (spec `40-arbitraitor-integration.md` §9.6, §6.7):
  `MediatedWorker::spawn` probes
  `arbitraitor_sandbox::compute_effective_controls(SandboxMode::Restricted, platform)`
  as a capability preflight, records the controls matrix + verdict into
  `WorkerPreflight` for the run state, and fails closed — typed
  `MediationError::UnavailableControls` naming each missing control, typed
  `UnsupportedPlatform` on non-Linux (ADR-0024; no non-secure bootstrap
  mode) — before any execution surface exists. `MediatedWorker::run_bash`
  routes scripts through `arbitraitor_exec`'s mediated bash
  (`ExecutionContextBuilder` via `ScriptExecution`) under an explicit
  network-denied `ExecutionPolicy`; errors translate Arbitraitor `ExecError`
  into log-safe static reason codes (no command output/args leakage, spec
  §9.23.4). Documented in `docs/sandbox-mediation.md` and in the README.
- GitHub App service identity (`arbsec-agent`) token-minting path (spec
  `10-orchestrator.md` §9.25.2, §9.41; issue #307). Layered config gains
  `github_app.slug` (built-in default `arbsec-agent`), `github_app.client_id`,
  `github_app.installation_id`, `github_app.private_key_uri` (`secret://` URI,
  fail-closed resolution — no ambient-credential fallback), and top-level
  `service_identities` (default `["arbsec-agent"]`). `orchestraitor-core`
  gains `GitHubAppAuth`: RS256 JWT minting with `iss = <client_id>` (GitHub
  rejects the numeric app ID with 401) and `exp = iat + 10min`, an
  installation-token cache re-minting at `expiry − 5min` behind a
  single-flight mutex+condvar so concurrent requests never duplicate mints,
  response-contract validation (1h lifetime cap, parsable RFC 3339 expiry),
  and redacted `Debug`/error output (no PEM/JWT/token material, spec
  `40-arbitraitor-integration.md` §9.23.4). `secret.rs` gains exact-store
  `SecretUri::resolve` (env + OS keyring behind the default `secrets-keyring`
  feature). New CLI subcommand `orc github mint-token` prints only non-secret
  metadata (installation id, expiry, SHA-256 fingerprint prefix). Documented
  in `docs/cli/orc-github.md` and README (#307).
- Documented the hidden test/debug override `ORCHESTRAITOR_GITHUB_API_ENDPOINT`
  (`--github-api-endpoint`) for `orc github mint-token`, including the security
  note that it redirects where the App JWT bearer is sent
  (`docs/cli/orc-github.md`) (#307).
- Static heuristic role routing for the six built-in orchestration roles (`explore`,
  `research`, `plan`, `implement`, `review`, `verify`) per spec `30-model-routing.md`
  §9.45 and spec §9.19.2: `roles.<id>.routing.*` keys are first-class configuration
  (known-key reporting, layered merge, `orc config get roles.implement.routing.provider`
  resolves out of the box via shipped built-in defaults for the Neuralwatt GLM-5.2
  single-provider bootstrap of spec §10.3). `orc routing resolve --role <id> [--json]`
  resolves a role through the layered configuration and persists a routing decision
  record (role, provider, model, precedence path, fallback reason) to the local
  `SQLite` store (`<config-dir>/routing.db`, WAL, `schema_migrations`-versioned).
  Incomplete effective entries fail closed with a typed error naming the missing
  configuration key; a role with no entry in any layer resolves via the documented
  bootstrap default and records the fallback reason. No decision model, no custom
  roles, no subscription awareness (E2) (#309).
- `orchestraitor-board` crate and `orc board` command: the bootstrap GitHub Projects v2
  board provider (spec `10-orchestrator.md` §9.43, §9.40; #308). `orc board ready [--json]`
  lists OPEN leaf Task/Bug items with `Target=MVP`, `Status=Ready`, and no unresolved
  `blockedBy` edges on the shared board (a closed issue is excluded with a warning even when
  its board fields say Ready/MVP; fail-closed on truncated
  `blockedBy`/field/label windows;
  malformed items are skipped with a warning, never a crash), and `orc board move <issue>
  --status "<Status>"` resolves the item across all configured repositories (ambiguous
  same-numbered issues fail with a typed reference error), writes the Status single-select
  field and verifies the write by
  read-back. Node IDs resolve at runtime from the human-readable names in
  `.agents/project/github-project.local.toml` and are cached under
  `$XDG_CACHE_HOME/orchestraitor/` — never inside the repository. Auth is injected via the
  `BoardAuth` trait; the bootstrap `SecretUriAuth` stub resolves the configured
  `secret://` URI (env-backed) and never sniffs ambient credentials; a hidden
  `--github-graphql-endpoint` override serves GHES instances.

### Changed

- The tree-sitter baseline indexer (`orchestraitor-context`) now builds against
  tree-sitter 0.27.0 (previously 0.26.11) to align with the Arbitraitor 0.27
  migration; the bump pulls in upstream's UTF-16 buffer over-read fix.

### Fixed

- `orc github commit-author` authenticates `GET /app` with a freshly minted
  App JWT instead of an installation token: `GET /app` is an App-level
  endpoint that GitHub answers with 401 for installation tokens, so the
  subcommand previously could never deliver its documented output. The bot
  user id is resolved with a follow-up `GET /users/{slug}[bot]` authenticated
  with the installation token (the `GET /app` payload carries the slug but
  not the bot user; GitHub rejects the App JWT on `GET /users`, and on an
  Enterprise Managed Users organization the bot profile is not publicly
  visible, so an unauthenticated request would answer 404). The JWT is
  secret material under the same guarantees as the token: held in memory
  only, injected solely into the one `Authorization` header, never printed,
  logged, or persisted.
- `orc github gh-env` now rejects a child command that exists but lacks the
  executable bit BEFORE minting a token; previously the token was minted and
  then the spawn was guaranteed to fail.
- Skill-script service-identity routing (`orc_lib_gh_service` in the
  github-project-workflow / github-pr-lifecycle skills) now routes ALL
  `gh` call sites that flow through the mutating scripts — the write
  commands (merge-gate, claim-issue assignment, release-issue,
  create-blocker, create-follow-up, decompose-issue) AND their per-script
  precondition reads (issue view/list before a mutation, the create-blocker
  cross-repo node-id lookups) — through the App installation token; only
  the read-only helper `orc_lib_gh` stays on the ambient `gh` auth. The
  caller identity for assignee-ownership checks is resolved from
  `orc github commit-author` on the service path (`gh api user` is
  user-context only and fails with an installation token); the
  `gh api user` probe remains only on the labelled personal fallback. The
  github_app config probe requires all three keys (`client_id`,
  `installation_id`, `private_key_uri`), distinguishes config-absent
  (labelled personal fallback) from a config resolution error (typed
  failure — never a silent personal-auth fallback), and the gh-env handoff
  no longer passes a stray `command` token into the child argv, which made
  every routed call fail. A failed `orc config get
  github_app.enforcement` read fails closed instead of permitting the
  personal fallback (an unset key still falls back in recommended mode).
- An invalid `ORC_GITHUB_APP_ENFORCEMENT` value in the skill-script wrapper
  (anything other than `recommended` or `required`, e.g. `Required`) now
  fails closed with a typed config error: previously an unrecognized pin
  compared false against `required` and — with the `github_app` config
  absent — silently took the labelled personal fallback, the fail-open
  outcome the pin exists to prevent.
- The skill-script caller-identity resolver
  (`orc_lib_resolve_my_login`) now picks the ambient-vs-service route with
  the SAME `github_app` config probe as the service wrapper instead of the
  `orc` binary's presence: with `orc` installed but the `github_app` config
  absent, mutating operations take the labelled personal fallback while the
  identity resolver previously demanded the bot login (which the
  mis-matched route could not resolve), breaking claim/release on exactly
  the machines the fallback exists for. The config-absent case now resolves
  the personal login via `gh api user`, matching the route the writes take;
  a config-probe resolution error yields an empty identity (typed failure,
  never a wildcard match).
- `orc github api` `-f/--field` now behaves like `gh api -f/--raw-field`:
  the value is ALWAYS sent as a string (previously values that happened to
  parse as JSON — `123`, `null`, `{"x":1}` — were sent as that JSON type,
  corrupting comment bodies that are valid JSON). GET/DELETE requests
  encode `--field` pairs as percent-encoded URL query parameters (the `gh
  api` shape) instead of silently dropping them into a JSON body the
  endpoint ignores, and `--input` on GET/DELETE is a typed error rather
  than a body GitHub never reads.
- `orc routing resolve` no longer dirties `git status` when run at the default store
  path: `.orchestraitor/routing.db` and its WAL sidecars are gitignored. Routing
  decision store errors now include the underlying cause in their `Display`
  output (#309).
