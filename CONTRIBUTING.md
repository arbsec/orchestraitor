# Contributing to Orchestraitor

Thank you for helping build a security-first coding-agent harness. Orchestraitor is secured by
[Arbitraitor](https://github.com/arbsec/arbitraitor); **every Orchestraitor contribution is
part of the integration attack surface**, and every security primitive belongs in Arbitraitor.

## Developer Certificate of Origin

All contributions must be signed off with the [Developer Certificate of
Origin](https://developercertificate.org/). Use `git commit -s`, or add
`Signed-off-by: Your Name <your.email@example.com>` to your commit. AI-generated contributions
are attributed to the human submitter, who remains responsible for the change regardless of
which tool produced it.

## Before contributing: where does the work belong?

Two repositories share one security model. Pick the right home **before** opening a PR:

- **Arbitraitor (`arbsec/arbitraitor`)** owns: sandboxing, policy evaluation, command/package/
  plugin/artifact inspection, network and secret enforcement, plan-bound approvals, output
  classification and promotion authorization, provenance, and security receipts. A missing
  security capability for Orchestraitor MUST be added to Arbitraitor first (`AGENTS.md`,
  spec §16.2).
- **Orchestraitor (`arbsec/orchestraitor`)** owns: the agent loop and session lifecycle,
  provider/harness adapters, the CLI/TUI/GUI/IDE/MCP/ACP surfaces, the context compiler,
  format-on-write and transaction orchestration, workspace lifecycle, and the presentation of
  Arbitraitor decisions/receipts to the user.

An Orchestraitor PR that needs a new security behavior is blocked on an Arbitraitor issue/PR.
Link them; do not implement a duplicate in Orchestraitor.

## Getting started

The application code does not exist yet (pre-implementation). You can still contribute to
specification, governance, and documentation:

1. Fork and clone the repository.
2. Read [`AGENTS.md`](AGENTS.md) and [`docs/spec/spec.md`](docs/spec/spec.md).
3. Create a worktree — **never commit directly to `main`**:

   ```sh
   git fetch origin
   git worktree add -b <type>/<slug> ../orchestraitor-<slug> origin/main
   ```

4. Once a Rust workspace exists, install tooling with `mise install` (pinned versions via
   `mise.toml`) and run the [pre-PR gate](AGENTS.md#rust-conventions).

## Workflow

1. **Only the project-manager agent selects work** during autonomous delivery (spec §9.33.1).
   Outside autonomous runs, pick an issue labeled `Target = MVP`, `Status = Ready`, with no
   unresolved blockers (see [workflow policy](.agents/project/orchestraitor-workflow.md)).
2. **Only leaf Tasks and Bugs are directly implementable.** Decompose Epics and Features into
   leaf work first, using native GitHub sub-issues; use native issue dependencies for blocking.
3. **One independently testable, revertible concern per PR.** Never hide newly discovered work
   inside a larger PR — open a separate issue/PR for follow-ups (spec §9.33.2).
4. **Check for conflicting in-flight work** before starting (open issues/PRs touching the same
   spec section or crate).
5. **Run pre-PR checks** (all must pass once `Cargo.toml` exists):

   ```sh
   cargo fmt --check
   cargo clippy --workspace --all-targets --all-features -- -D warnings
   cargo check --workspace --all-targets --all-features --locked
   cargo nextest run
   rumdl check .
   cargo deny check
   cargo audit
   ```

6. **Open a PR** with a Conventional Commits title, linked issue, spec references, security
   impact, test evidence, and documentation impact. Complete the
   [PR checklist](.github/PULL_REQUEST_TEMPLATE.md).
7. **Adversarial review by a different agent/session is mandatory** (spec §21.1). Reviewers use
   fresh contexts; each new commit re-runs review against the current HEAD. Security-sensitive
   changes require human review before release.
8. **Squash merge** on green CI with all review threads resolved. Clean up your worktree.

## Conventional Commits

PR titles follow [Conventional Commits](https://www.conventionalcommits.org/). Branch names are
`<type>/<short-slug>` (e.g. `feat/provider-proxy`, `security/arbitraitor-wiring`).

```text
feat(provider-proxy): add Anthropic Messages facade
fix(workspace): detect base-branch drift before promotion
security(arbitraitor-client): require effective-control probe before spawn
docs(spec): clarify context provenance envelope
```

## Code style

- **Rust 2024 edition.** `rustfmt` and `clippy` are enforced in CI and git hooks.
- No `unwrap()`/`expect()` in production code. No `as any`/`@ts-ignore` equivalents, no blanket
  `#[allow(...)]`.
- No `unsafe` in Orchestraitor crates where avoidable; OS-specific unsafe security code belongs
  in isolated **Arbitraitor** crates with explicit safety invariants (spec §21.2.7).
- Newtypes for security-relevant values (hashes, identities, digest-based optimistic
  concurrency).
- Match Arbitraitor conventions: `thiserror` 2 at library boundaries, `miette` at the CLI
  boundary, errors and traces never leak secrets (spec §9.23.4).

## Dependencies

Adding a production dependency is a security-relevant decision. License must be `MIT`,
`Apache-2.0`, `BSD-3-Clause`, `ISC`, or `Apache-2.0 WITH LLVM-exception` (tech-stack.md §18).
Pin exact minor versions, isolate third-party crates behind project-owned traits
(`ProviderTransport`, `AgentAdapter`), and run `cargo-deny` + `cargo-audit` + `cargo-vet`.
Never add a dependency without justification in the PR.

## Documentation

Any change to **public behavior** updates human-facing docs in the same PR — README, CLI
reference, configuration docs, `CHANGELOG.md` `[Unreleased]`. Generated `cargo doc` and code
comments alone do not satisfy this requirement (spec §9.17.1, §9.33).

## Questions?

Open a [GitHub Discussion](https://github.com/arbsec/orchestraitor/discussions) for general
questions. For security issues, see [SECURITY.md](SECURITY.md) — **do not use public issues**.
