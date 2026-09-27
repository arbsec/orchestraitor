# Changelog

All notable consumer-visible changes to Orchestraitor are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html) once it begins tagging releases.

> **Scope:** release-note entries serve consumers of Orchestraitor (people who build, run,
> or integrate `orc` / `orcd` / the crates). Internal development notes — spec-section
> bookkeeping, repository tooling, agent workflows, review process, issue/task tracking —
> live in pull-request descriptions, spec documents, and evidence files, never here.

## [Unreleased]

### Fixed

- `orc routing resolve` no longer dirties `git status` when run at the default store
  path: `.orchestraitor/routing.db` and its WAL sidecars are gitignored. Routing
  decision store errors now include the underlying cause in their `Display`
  output (#309).

### Added

- `orc campaign run --once [--json]` and the `orchestraitor-campaign` crate: the one-shot
  campaign pass (spec `10-orchestrator.md` §9.35, `60-milestones.md` MVP-1; #313). Each
  pass reads the reconciled board through the #308 provider, applies the minimal
  epic-focus rule (P0-labelled items first, stable `(repo, issue-number)` order), selects
  at most one eligible task, persists exactly one append-only decision record to the
  local SQLite store (`<config-dir>/campaign.db`, `schema_migrations`-versioned), and —
  for a selection — spawns the worker via the daemon-less direct path (deterministic
  repo-scoped `board-<owner-repo>-<number>` task id). No-op passes persist typed reasons
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
