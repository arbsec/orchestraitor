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
- The `board.query` coordinator decision tool (spec `10-orchestrator.md`
  §9.39, §9.40, §9.43; #332): a read-only, typed query over a `BoardProvider`
  with two modes — a conjunctive filter search (item type, status, typed
  field values) returning typed items (id, type, title, status, fields,
  dependency edges), never raw provider JSON; and a blocked-graph mode that
  walks the board's native `blockedBy` edges transitively with back-edge
  cycle detection (a diamond DAG is not a cycle; a genuine cycle is surfaced
  as a typed board-data-corruption indication per §9.40, never looped).
  Every invocation is recorded in the event store as a `ToolRequest` event
  carrying the tool name, the filters as data (string values truncated), a
  result summary, and the §9.25.1 delegation chain labeled
  `chain_source: client-asserted` with a `claimed:` prefix per label — the
  tool never fabricates verified identity. In this slice the event store is
  per-invocation (written and hash-chain-validated, not yet durable);
  persistence lands with the daemon event-store wiring (§9.17). Filter
  values are untrusted input (§6.1): instruction-shaped values are matched
  as inert data and never executed. Surfaced as `orc board query` (against a
  deterministic in-memory fixture board in this slice — the sqlite provider
  is the #318 follow-up) and as the `board.query` MCP gateway tool when a
  board provider is configured; when none is, the gateway's tool router
  disables the route (hidden from `tools/list`, calls rejected). Documented
  in [docs/cli/orc-board.md](docs/cli/orc-board.md).
- Owner quickstart for the `orc loop` bootstrap runner in the mdBook
  ([Bootstrap Loop Quickstart](book/src/getting-started/loop-quickstart.md)): prerequisites
  (board config, explicit board auth, optional GitHub App service identity), first
  invocation, what a normal pass does, how to read `loop.db` rows and the end-of-run
  summary, stall/timeout/budget-stop semantics, safe shutdown, and the fixed bootstrap
  guard set. Served on GitHub Pages with the rest of `book/` (the loop runner itself lands
  with #434). Documented at
  [book/src/getting-started/loop-quickstart.md](book/src/getting-started/loop-quickstart.md)
  and linked from the README.
- GitHub Pages documentation site (the mdBook under `book/` builds and deploys
  via the previously disabled `docs.yml` workflow): the book introduction now
  reflects the loop-first product framing and links to the rendered repo docs
  instead of broken out-of-tree paths. Site goes live on the first push to
  `main` that touches `book/**` or `docs/**`.

### Changed

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
  The deterministic table-driven `FixtureDecisionProvider` ships as the only
  implementation and conformance target; the TypeSafe/jev adapter stays
  default-off until its license is allowlisted (tech-stack §17, §18) and no
  network calls are made anywhere. Router integration is **default off**
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
  errors for partial entries, and the `neuralwatt`/`glm-5.2` bootstrap
  fallback. Role ids follow the provider id shape (1–64 lowercase ASCII
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
