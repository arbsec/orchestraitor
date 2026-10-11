# Workflows

This directory contains the CI workflows for Orchestraitor. Four of them —
[code](code.yml), [coverage](coverage.yml), [invariants](invariants.yml), and
[security](security.yml) — constitute the **parity gate**: the set of checks that
any change to the Rust workspace must pass, as defined in
[`docs/spec/tech-stack.md` §15](../../docs/spec/tech-stack.md) and cited by the
[workflow policy](../../.agents/project/orchestraitor-workflow.md)
("Forbidden administrative shortcuts").

## The parity gate

| Workflow | Enforces | Trigger paths |
|---|---|---|
| [code.yml](code.yml) | `cargo fmt --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo check --locked`, `cargo nextest run --workspace --all-features`, `cargo doc` | `**/*.rs`, `**/Cargo.toml`, `Cargo.lock`, `.mise.toml`, itself |
| [coverage.yml](coverage.yml) | Full-workspace test run with LCOV upload to Qlty Cloud (no threshold enforced yet) | `**/*.rs`, `**/Cargo.toml`, `Cargo.lock`, itself |
| [invariants.yml](invariants.yml) | Security invariant property tests (`cargo nextest run --workspace`) | `**/*.rs`, `**/Cargo.toml`, `Cargo.lock`, `.mise.toml`, itself |
| [security.yml](security.yml) | `cargo deny check` (licenses, bans, sources, advisories) and `cargo audit` | `**/Cargo.toml`, `Cargo.lock`, `deny.toml`, `.mise.toml`, itself |

All four run as required checks on `push` to `main` and on every `pull_request`
that touches the paths above. Every `**/Cargo.toml` and `Cargo.lock` change
therefore exercises the whole gate automatically — no workflow file needs to
change for an ordinary dependency bump.

Supporting workflows, outside the gate:

- [codeql.yml](codeql.yml) — CodeQL security analysis; runs on all pushes to
  `main` and all PRs (no path filter).
- [docs.yml](docs.yml) — publishes the `book/` and `docs/` sites to GitHub Pages.
- [markdown.yml](markdown.yml) — `rumdl check` over `**/*.md`.
- [release.yml](release.yml) — manual release pipeline (`workflow_dispatch` only).

## When a `Cargo.toml` change requires workflow updates

The existing path triggers suffice whenever the manifest change stays within
shapes the gate already exercises: version bumps of existing dependencies
(workspace or per-crate), adding or removing crates in the existing workspace,
and feature/dependency edits that `cargo clippy`, `cargo check --locked`,
`cargo nextest`, `cargo deny`, and `cargo audit` evaluate as-is. Precedent:
PRs #515 and #544 merged workspace `Cargo.toml` dependency bumps with no workflow
changes — the gate ran on the manifest paths and that is the intended behavior.

A workflow update is required **in the same PR** only when the manifest change
introduces a dependency or build shape that no existing workflow exercises, for
example:

- a new tool or linter whose check must gate merges (add a job to the relevant
  workflow, and its path to that workflow's own trigger path);
- a new crate type or target platform outside the current build/test matrix;
- a dependency class that `cargo-deny`/`cargo-audit` do not yet police
  (update `deny.toml` and [security.yml](security.yml) together).

## How the rule is evaluated in review

The policy rule — "No committing a `Cargo.toml` workspace without simultaneously
adding the parity-gate workflows" — is checked as follows. The reviewer asks a
single question about the changed `Cargo.toml` surface:

> Does this change introduce a dependency or build shape that no existing
> parity-gate workflow exercises?

- **No** (the common case: dependency bumps, crate additions, feature edits) —
  the existing `**/Cargo.toml` / `Cargo.lock` path triggers run the full gate;
  no workflow update is required. Flagging such a PR for missing workflow files
  is a false positive.
- **Yes** (new tool, new platform, unpoliced dependency class) — the workflow
  update MUST land in the same PR; the manifest change does not merge without it.

If a reviewer cites this rule, they MUST point at the concrete trigger paths
above and name the uncovered shape; a citation without either is not actionable.
