# `orc init`

`orc init` performs deterministic, local project detection and proposes
`.orchestraitor/orchestraitor.toml`. It does not need a configured model provider, does not call
an LLM, and never prompts for an API key during initialization (spec §9.20).

Detected signals include languages, formatters, package managers, Git layout, Dev Container and
toolchain files, existing agent/MCP/IDE configuration, sensitive paths, and likely generated files
(spec §9.21). Uncertain domain classification enables the required `general` domain instead of
guessing.

```sh
orc init
orc init --dry-run
orc init --project /path/to/repo
```

`--dry-run` prints the proposed TOML and summary without writing any files. Normal mode creates
`.orchestraitor/orchestraitor.toml`; each proposed entry is marked with `# Proposed by orc init` so
users can accept, amend, or reject it before relying on it (spec §9.22.6).

The summary reports detected signals and uncertain areas. Provider setup is only an optional next
step; features that later require a provider surface a labeled configuration affordance at that
time, not during init.
