# Changelog

All notable changes to Orchestraitor are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html) once it begins tagging releases.

## [Unreleased]

### Added

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

[Unreleased]: https://github.com/arbsec/orchestraitor/compare/HEAD
