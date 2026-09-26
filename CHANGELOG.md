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

### Fixed

- `SecretResolveError::KeyringLookup` no longer renders the keyring backend
  error via its derived `Debug` (keyring-core payload variants embed the raw
  retrieved secret bytes); the source now appears as a redacted marker while
  the miette `Display`/`source()` chain stays intact and payload-free (#307).
- `GitHubAppAuth` token cache: a panic mid-mint now resets the single-flight
  slot and wakes waiters instead of leaving the cache wedged in the
  `Minting` state (#307).
- Lockfile refresh for yanked and advisory-flagged crates so supply-chain checks pass again:
  `chacha20 0.10.1 → 0.10.2` (0.10.1 yanked), `h2 0.4.15 → 0.4.19` (RUSTSEC-2026-0258),
  `rustls 0.23.43 → 0.23.45` (RUSTSEC-2026-0285), `faster-hex 0.10.0 → 0.10.1`
  (RUSTSEC-2026-0306). All bumps stay inside the existing semver ranges.
- `orchestraitor-context` index is keyed by blob digest instead of path: a file move with
  unchanged content is recognized as reuse rather than a reparse, and files deleted from the
  tree no longer remain queryable after reindex.
- `orchestraitor-core` merges dynamic configuration table entries field-by-field and includes
  structured error causes and source chains; sensitive tracing fields are omitted entirely.
- `orchestraitor-cost-ledger` removes `BudgetScope::Organization` and `BudgetScope::User`
  variants whose filter previously matched every ledger row, silently breaking budget
  isolation; the variants are deferred until per-org/per-user attribution columns ship, and the
  remaining scopes gain regression tests pinning scope isolation.
- `orchestraitor-daemon` content-addressed reads recompute SHA-256 over returned bytes and
  refuse corrupted or out-of-band-written blobs (`StoreError::DigestMismatch`); adversarial
  tests pin that hash-chain validation rejects tampered event records.

[Unreleased]: https://github.com/arbsec/orchestraitor/compare/HEAD
