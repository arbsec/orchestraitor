# Changelog

All notable changes to Orchestraitor are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html) once it begins tagging releases.

## [Unreleased]

### Added

- `orc verify`, `orc policy check`, `orc run --non-interactive`, and
  `orc evidence export` subcommands implementing headless and CI support
  per spec MVP-8 (§998). All machine-oriented commands support `--json`,
  `--quiet`, and stable exit codes (0 success, 2 config error, 3 verification
  failure, 4 security block, 5 infrastructure failure). `orc verify` detects
  the project-configured verification registry from `orchestraitor.toml`
  `[[verification.commands]]` entries and recognized configuration files
  (Cargo.toml, package.json, pyproject.toml, go.mod, pom.xml); the same
  registry works locally and in CI. Verification command execution requires
  an Arbitraitor sandbox (spec §6.7, §16.2) and fails closed until available.
  `orc policy check` delegates evaluation to Arbitraitor via
  `ArbitraitorClient::evaluate_policy` and reports the verdict in JSON;
  `--shadow` mode reports what would have happened without enforcement.
  `orc run --non-interactive` sets up the non-interactive execution context
  with approvals defaulting to block. `orc evidence export` produces a
  privacy-preserving archive using the tamper-evident hash-chained event
  store; secrets, prompts, completions, tool arguments, and MCP payloads are
  always redacted (spec §9.17.1). New `VerificationConfig` and
  `VerificationCommand` config schema types in `orchestraitor-core` for the
  project-configured verification registry.
- `orchestraitor-provider-neuralwatt` crate implementing `ProviderTransport` against
  the Neuralwatt OpenAI Chat Completions-compatible API for GLM-5.2 BYOK (spec §10.3).
  Default base URL `https://api.neuralwatt.com/v1` (overridable via config); API key
  resolved from `secret://keyring/neuralwatt` or `NEURALWATT_API_KEY` env var
  (tech-stack §3.2). Streaming via `reqwest::Response::bytes_stream()` with SSE parsing
  into `ModelEvent` values. Per-call cost entries emitted per spec §9.19.4 through a
  `CostSink` trait. Wire-level cassette tests for `/v1/models` and
  `/v1/chat/completions` (streaming, non-streaming, and tool calls). The legacy Zhipu
  endpoint `open.bigmodel.cn` is rejected at configuration time (spec §10.3).
- `orchestraitor-daemon` crate with a SQLite WAL metadata store, schema migrations,
  hash-chained event persistence, Arbitraitor receipt/backlog/delegation tables, and an
  Arbitraitor-compatible SHA-256 filesystem CAS for spec §9.17 and tech-stack §11.
- `orchestraitor-context` crate with a content-addressed tree-sitter baseline indexer,
  Appendix E context query API, and spec §9.15.1 provenance envelopes on every emitted item.
- Initial repository governance, contribution guidance, security policy, code of conduct, and
  support documents, adapted from the sibling Arbitraitor repository for Orchestraitor's
  spec-driven, security-first workflow.
- Root [`AGENTS.md`](AGENTS.md) with always-active project rules: Arbitraitor as the exclusive
  security authority (spec §2.2, §16), spec as source of truth, engineering priorities, scope
  and GitHub workflow, review/merge invariants, documentation, testing, and Rust conventions.
- Dual `MIT OR Apache-2.0` licensing, matching Arbitraitor.
- `.github/` scaffolding: `CODEOWNERS`, `dependabot.yml`, issue templates
  (`bug.yml`, `feature.yml`, `task.yml`, `spike.yml`, `config.yml`), a pull-request template
  with stable machine-readable checklist markers, and a workflows `README.md` documenting the
  intended CI layout once application code exists.
- `.agents/project/orchestraitor-workflow.md` project workflow policy: MVP-only scheduling,
  security-first review requirements, Arbitraitor ownership boundaries, cross-repository
  blocker handling, required review domains, documentation and testing expectations, and PR
  convergence requirements.
- `.agents/project/github-project.example.toml` example GitHub Project configuration
  (resolves mutable node IDs at runtime; commits only human-readable names).
- Two reusable Agent Skills under `.agents/skills/`:
  `github-project-workflow` (triage, decomposition, ready-queue, claims, reconciliation) and
  `github-pr-lifecycle` (draft PR, CI inspection, adversarial review convergence, merge
  eligibility, post-merge reconciliation), each with `references/` and composable `scripts/`.
- `orchestraitor-provider-proxy` crate with OpenAI Chat Completions, OpenAI Responses,
  Anthropic Messages, `/v1/models`, short-lived local tokens, upstream BYOK credential
  isolation for child processes, per-completion cost attribution, and explicit Mode D
  trust-boundary reporting per spec §10.1.

### Fixed

- `orchestraitor-context` index is now keyed by blob digest instead of path: a file move to a
  new path with unchanged content is recognised as reuse, not reparse. Paths present in the
  previous index but absent from the new traversal are also evicted on reindex, so deleted
  files no longer remain queryable. Reference records now carry the provenance of the blob
  that owns them (the reference-occurrence blob, not the target symbol's provenance).
  The Appendix E query API exposes `repository_summary`, `symbol_body`, `related_tests`,
  `diagnostics`, and `expand_context` as MVP stubs pending §9.16 LSP-backed wiring.
  Cross-blob reference keying is deferred; a TODO in `index.rs` flags the digest-keyed
  follow-up for a future iteration.
- `orchestraitor-core` now merges dynamic configuration table entries field-by-field, includes
  structured error causes and source chains, and omits sensitive tracing fields entirely.
- `orchestraitor-cost-ledger` no longer exposes `BudgetScope::Organization` or
  `BudgetScope::User`. The `scope_filter` previously mapped both variants to the SQL
  tautology `?1 = ?1`, so a budget configured for one org or user silently counted every
  ledger row, breaking isolation. The variants are deferred until `cost_entries` ships
  real per-org / per-user attribution columns. Project, Session, Domain, and Agent
  scopes continue to filter on their own columns and gain explicit regression tests
  pinning scope isolation.
- `orchestraitor-daemon` `CasDirectory::load_bytes` now recomputes SHA-256 over the bytes it
  reads and refuses to return them if the digest does not match the address; previously a
  corrupted or out-of-band-written blob would be returned as-is, undermining the
  content-addressed guarantee. A new `StoreError::DigestMismatch { expected, actual }`
  variant reports both digests; an adversarial test (`cas_load_bytes_rejects_corrupted_blob`)
  pins the behaviour against on-disk corruption.
- `orchestraitor-daemon` `DaemonStore::load_event_records` is now exercised by adversarial
  tests (`load_event_records_rejects_tampered_record_json`,
  `load_event_records_rejects_record_json_payload_drift`) that mutate `event_records.record_json`
  via raw `SQL` to confirm the hash-chain validator rejects the drift with
  `EventError::RecordHashMismatch`. A `pub(crate)` `execute_raw` test hook is the only path
  that can bypass the typed CRUD helpers; it is `#[cfg(test)]` and documented as such.

[Unreleased]: https://github.com/arbsec/orchestraitor/compare/HEAD
