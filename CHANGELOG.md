# Changelog

All notable changes to Orchestraitor are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html) once it begins tagging releases.

## [Unreleased]

### Added

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
- `orchestraitor-mcp` crate with an `rmcp` gateway skeleton, built-in filesystem/workflow tool
  surface, canonical `.agent/mcp.toml` schema loader, and per-session MCP drift fingerprinting
  data model for spec §9.5, §9.18.1, and §17.2.

### Fixed

- `orchestraitor-core` now merges dynamic configuration table entries field-by-field, includes
  structured error causes and source chains, and omits sensitive tracing fields entirely.
- `orchestraitor-cost-ledger` no longer exposes `BudgetScope::Organization` or
  `BudgetScope::User`. The `scope_filter` previously mapped both variants to the SQL
  tautology `?1 = ?1`, so a budget configured for one org or user silently counted every
  ledger row, breaking isolation. The variants are deferred until `cost_entries` ships
  real per-org / per-user attribution columns. Project, Session, Domain, and Agent
  scopes continue to filter on their own columns and gain explicit regression tests
  pinning scope isolation.
- `orchestraitor-mcp` `ProjectScope::from_root` now derives `ProjectId` from a SHA-256
  digest of the canonical root path (with an optional explicit
  `.orchestraitor/project-id` override) instead of the root basename, so two same-named
  roots in different parents no longer share a project id and `require_server_project`
  cannot accept a server registered for a different project. The basename is preserved
  as a human-readable display label via the new `ProjectScope::display_label`.
- `orchestraitor-mcp` `DriftFingerprint::build` now sorts tool schemas by
  `(name, description, canonical input_schema)` before hashing, so equivalent tool sets
  declared in different orders produce the same schema digest.
- `orchestraitor-mcp` gains the §9.18.1 renewed-trust comparison primitive
  `DriftFingerprint::compare`, returning the `FingerprintChange` enum
  (`NoChange`, `ExecutableChanged`, `SchemaChanged`, `CapabilityExpanded`,
  `CapabilityReduced`) in security-severity order.

[Unreleased]: https://github.com/arbsec/orchestraitor/compare/HEAD
