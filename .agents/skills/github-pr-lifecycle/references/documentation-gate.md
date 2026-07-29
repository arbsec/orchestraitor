# Documentation gate

Any change to **public behavior** updates human-facing docs in the same PR (spec §9.17.1, §9.33, AGENTS.md). This reference defines what counts as public behavior and how `classify-docs-impact` classifies it.

## Public behavior surfaces

A change touches public behavior if the diff affects any of:

| Surface | Detection pattern (file paths / content) |
|---|---|
| CLI/TUI behavior | `crates/orchestraitor-cli/`, `crates/orchestraitor-tui/`, clap subcommands/flags, exit codes |
| Configuration | `orchestraitor.toml` schema, `[providers]`, `[normalization]`, `[data_classification]`, `[profiles]`, `[data_governance]`, ConfigField descriptions |
| Environment variables | `ORCHESTRATOR_*`, `NEURALWATT_API_KEY`, `ZHIPU_API_KEY`, any `*_API_KEY` referenced in code |
| Public APIs | `pub fn`, `pub struct`, `pub trait`, `pub enum` in `crates/orchestraitor-{core,model,events,adapter-api,provider-api,mcp}/` |
| Daemon protocols | JSON-RPC method names/schemas in `crates/orchestraitor-daemon/`, `crates/orchestraitor-provider-proxy/` |
| Built-in tools | `fs.*`, `format.run`, `lint.run`, `check.run`, `test.run`, `task.run` (spec §9.5/MVP-6) |
| MCP behavior | `crates/orchestraitor-mcp/`, tool schema, namespacing, lifecycle |
| Provider support | `crates/orchestraitor-provider-*/`, new/changed provider configs, `[[providers]]` blocks |
| Security guarantees | `SECURITY.md`, `AGENTS.md` security rules, capability report surfaces |
| Error behavior | error code registry (`ORC-*-NNN`), `orchestraitor-model` error enums, `miette::Diagnostic` impls |
| Installation/migration/removal | README install steps, `orc init`, `orc connect`/`disconnect`, `orc uninstall`, config migration |

## What does NOT require doc updates

- Internal refactors with no public API change (private function moves, test restructuring).
- Dependency version bumps with no behavior change (lockfile updates).
- CI workflow changes (`.github/workflows/`) — these are not public behavior.
- Code comment improvements (docstrings on private items).

Even in these cases, `CHANGELOG.md [Unreleased]` may still warrant an entry ("chore: bump deps", "refactor: move X to Y").

## `classify-docs-impact` output

The script reads `gh pr diff --name-only` + the diff body, matches against the patterns above, and outputs:

```json
{
  "public_behavior_changed": true,
  "touched_surfaces": ["CLI behavior", "Configuration"],
  "docs_required": true,
  "docs_updated_in_pr": null,
  "checklist_marker": "orc:docs"
}
```

The script does NOT verify the docs were actually updated — that is the reviewer's and `reconcile-checklist`'s job. It classifies the impact; the checklist marker `<!-- orc:docs -->` must be checked based on the classification + evidence of doc changes in the PR diff.

## `CHANGELOG.md [Unreleased]`

Every public-behavior change gains an entry in `CHANGELOG.md` under `[Unreleased]` in the same PR. The entry follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) format: `Added`, `Changed`, `Deprecated`, `Removed`, `Fixed`, `Security`.

## Generated docs do not count

Generated `cargo doc` output and `///` doc comments on public items are necessary but NOT sufficient. The requirement is **human-facing** documentation — README, CLI reference, configuration guide, security docs, CHANGELOG. A PR that only adds `///` comments to public functions but does not update README/CHANGELOG/config docs has NOT met the documentation gate for a public-behavior change.
