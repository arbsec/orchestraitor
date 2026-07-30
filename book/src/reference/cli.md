# CLI Reference

The `orc` binary is Orchestraitor's local control-plane CLI (spec §1.2). It is a thin client
to the `orcd` daemon and to layered project configuration. The long-form `orchestraitor`
executable is an alias for discoverability where `orc` conflicts with another installed
program.

> **Status:** Only `init`, `config`, and `models` are implemented today. The remaining
> subcommands are specified (spec §1.2, §9.x) and will be added as the MVP lands. This page
> marks implemented vs. planned so you never invoke a command that does not exist.

## Global options

These options apply to every subcommand and may also be set via environment variable:

| Flag | Env var | Default | Description |
|---|---|---|---|
| `--config-dir <PATH>` | `ORCHESTRAITOR_CONFIG_DIR` | `.orchestraitor` | Root for non-project config layers. |
| `--project-dir <PATH>` | `ORCHESTRAITOR_PROJECT_DIR` | `.` | Project directory containing `orchestraitor.toml`. |
| `--models-dev-endpoint <URL>` | `ORCHESTRAITOR_MODELS_DEV_ENDPOINT` | _(hidden)_ | Alternate models.dev catalog endpoint for mirrors and tests. |

`orc` runs with `arg_required_else_help`, so invoking it with no subcommand prints help and
exits non-zero.

## Implemented subcommands

### `orc init` — project detection and configuration proposal

Performs deterministic, local project detection and proposes
`.orchestraitor/orchestraitor.toml`. It does not need a configured model provider, does not
call an LLM, and never prompts for an API key during initialization (spec §9.20). Detected
signals include languages, formatters, package managers, Git layout, Dev Container and
toolchain files, existing agent/MCP/IDE configuration, sensitive paths, and likely generated
files (spec §9.21).

```sh
orc init                       # write .orchestraitor/orchestraitor.toml
orc init --dry-run             # print proposed TOML without writing
orc init --project /path/to/repo
```

| Flag | Description |
|---|---|
| `--dry-run` | Print the proposed TOML and summary without writing any files. |
| `--project <PATH>` | Project root to inspect (default `.`). |

Each proposed entry is marked `# Proposed by orc init` so users can accept, amend, or reject it
before relying on it (spec §9.22.6).

### `orc config` — configuration inspection and migration

Inspects, edits, validates, diffs, and migrates the layered configuration (spec §9.22.3,
§9.22.8). Layers: `project` (`orchestraitor.toml`), `user`, `org`, `dir`.

```sh
orc config get <key>
orc config explain <key>
orc config set <key> <value> [--layer=project|user|org|dir]
orc config unset <key> [--layer=project|user|org|dir]
orc config validate
orc config diff [--layer=project|user|org|dir] [--json]
orc config migrate
```

| Subcommand | Description |
|---|---|
| `get <key>` | Print a resolved value. |
| `explain <key>` | Print the resolved value, source layer, source file, inherited state, and profile contribution placeholder. |
| `set <key> <value> [--layer=L]` | Set a key at the selected layer. `<value>` is a TOML scalar/array/object literal, or a string when not valid TOML. |
| `unset <key> [--layer=L]` | Remove a key from the selected layer. |
| `validate` | Reject ambiguous same-layer conflicts (two shards under the same layer both defining the same key) and report unknown keys. |
| `diff [--layer=L] [--json]` | Show effective-vs-defaults or layer-specific differences; `--json` emits stable JSON. |
| `migrate` | Forward-only, comment-preserving migration using `toml_edit`; writes a `.bak.*` backup first. |

### `orc models` — models.dev catalog management

Manages the cached models.dev catalog (spec §9.22.8, §10.4).

```sh
orc models refresh    # force an immediate models.dev catalog fetch into the local cache
orc models rollback   # return to the previous cached snapshot without deleting manually configured models
```

## Planned subcommands (spec §1.2)

These are specified but **not yet implemented**. They are listed so the intended surface is
visible; do not rely on them until the MVP delivers them.

| Command | Spec | Purpose |
|---|---|---|
| `orc run <adapter>` | §10.1 | Run a native or wrapped agent (e.g. `orc run claude`, `orc run codex`). |
| `orc attach` | §9.18.2 | Attach to a running session. |
| `orc diff` | §9.5 | Review a compact transaction diff. |
| `orc history` | §9.17 | Inspect the event and receipt store. |
| `orc checkpoint` / `orc restore <node>` | §9.5 | Versioned transaction checkpoint and restore. |
| `orc branch <node>` / `orc compare <a> <b>` | §9.5 | Session branching and comparison. |
| `orc undo` / `orc redo` | §9.5 | Transactional undo/redo. |
| `orc capabilities [--json]` | §9.32.4, §16.7 | Print the Arbitraitor effective-control capability report. |
| `orc policy show` | §9.8 | Show the resolved Arbitraitor policy. |
| `orc doctor` | §9.32 | Diagnose platform capability and configuration health. |
| `orc status` | §9.1 | Daemon and session status. |
| `orc serve` | §9.1 | Run the `orcd` daemon (alternatively run the `orcd` binary directly). |
| `orc mcp stdio --project auto` | §17.2.2 | MCP gateway stdio transport. |
| `orc observe` / `orc wrap` / `orc connect` / `orc proxy` / `orc disconnect` | §9.18.2 | Incremental adoption ladder; `orc disconnect` restores prior state in under 30 seconds. |

## Daemon: `orcd`

The `orcd` binary is the durable supervisor (spec §9.1, §17.2.1). It is invoked directly,
not via `orc serve` (which is planned):

```sh
orcd /path/to/orcd.sock        # first positional arg is the socket path
# or
ORCHESTRAITOR_DAEMON_SOCKET=/path/to/orcd.sock orcd
# or
orcd                           # uses a temporary default path
```

`orcd` exposes JSON-RPC methods `initialize`, `health`, and `shutdown`. `SIGTERM` triggers
graceful shutdown within the five-second daemon budget (tech-stack §10).

## Exit codes

`orc` uses `miette` at the CLI boundary and returns standard process exit codes:

| Code | Meaning |
|---|---|
| `0` | Success. |
| `1` | Application error: invalid configuration, provider metadata failure, or a command-specific failure reported as a `miette` diagnostic. |
| `2` | Argument parse error (emitted by `clap` before the command runs). |

Commands that write files (`orc init` without `--dry-run`, `orc config set`, `orc config
migrate`) fail atomically: a write either completes or leaves prior state intact, and the
diagnostic identifies the failing layer or path.
