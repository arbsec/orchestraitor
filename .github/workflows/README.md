# GitHub Actions workflows

> **Pre-implementation.** This directory intentionally contains **no workflow files** yet.
> Orchestraitor ships no application code, so CI workflows that run `cargo` would have nothing
> to run against. A workflow added before `Cargo.toml` exists would be a fake green check,
> not a meaningful gate. This file documents the intended layout so the workflows can be added
> in lockstep with the first application code, matching the parity gate in
> [`docs/spec/tech-stack.md` §15](../../docs/spec/tech-stack.md) and `AGENTS.md`.

## Intended workflows (added with the first application code)

Workflow files mirror the Arbitraitor layout and use pinned-SHA actions only. Dependabot
proposes SHA bumps; a maintainer reviews each (`../../.github/dependabot.yml`).

| File | Purpose | Required on PR? |
|---|---|---|
| `code.yml` | `cargo fmt --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo check --workspace --all-targets --all-features --locked`, `cargo nextest run --workspace`, `cargo test --doc`. Linux + macOS matrix. | Yes |
| `docs.yml` | `rumdl check .`, `cargo doc --workspace --all-features --no-deps`, `cargo run -p xtask -- docs-check` (spec/docs freshness). | Yes |
| `markdown.yml` | Markdown lint and link validation for `docs/`, `*.md`. | Yes |
| `security.yml` | `cargo deny check` (advisories, license, bans, sources), `cargo audit`. | Yes |
| `codeql.yml` | CodeQL on `rust`. | Yes |
| `invariants.yml` | Spec-vs-code drift checks: the `xtask docs-check` invariants, generated-file freshness, config-schema validation via `orc config validate` once it exists. | Yes |

## Required pull-request checks (parity gate)

The required-status-check ruleset (configured once the repo has checks to require) MUST require
exactly these, all of them non-optional (`AGENTS.md` "Review and merge invariants"). A missing
or skipped check is treated as a failure, not a pass (spec §21.10):

```text
format                    cargo fmt --check
clippy                    cargo clippy --workspace --all-targets --all-features -- -D warnings
build                     cargo check --workspace --all-targets --all-features --locked
tests                     cargo nextest run --workspace
docs                      cargo test --doc
docs freshness            cargo run -p xtask -- docs-check
markdown                  rumdl check .
dependency policy         cargo deny check
advisories                cargo audit
CodeQL (rust)             github/codeql-action
```

Scheduled (not PR-blocking): extended Miri (`cargo miri`), fuzzing (`cargo-fuzz`, 5 min/target),
mutation testing (`cargo-mutants`), full coverage (`cargo-llvm-cov`), performance regression
benchmarks on pinned hardware, and live-provider contract tests on a trusted runner. Retries may
identify flaky tests but MUST NOT convert flaky behavior into a passing gate (spec §21.10).

## Ruleset conventions

- Required checks come from the **trusted base branch**, not the PR being reviewed.
- Merge strategy: **squash merge only**. No admin-merge bypasses (`AGENTS.md`).
- Branch protection on `main`: require PR review, require status checks above, require linear
  history, require signed commits (DCO), dismiss stale reviews on push, require CODEOWNERS
  review for the `@arbsec/security` paths in [`../CODEOWNERS`](../CODEOWNERS).
- No self-review; no force-push to `main`; no deletion of `main`.

## Why no workflows now

Adding a workflow file that runs against a non-existent workspace would either fail (a red
check on a repo with no code) or be configured to pass vacuously (a green check with no
meaning). Both are worse than documentation: a red check blocks all early governance PRs; a
green check violates "checkboxes are verified facts, not intentions". The parity gate is wired
up in the same PR that adds the first `Cargo.toml`.
