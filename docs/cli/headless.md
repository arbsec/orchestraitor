# Headless and CI commands (spec MVP-8)

All core workflows operate interactively and non-interactively. Stable JSON
output and documented exit codes are required for automation (spec §998 MVP-8).

## Commands

```sh
orc verify                    # run project-configured verification
orc policy check              # evaluate policy against a plan or session
orc run --non-interactive      # run a task without TUI interaction
orc evidence export            # export session evidence (privacy-preserving)
```

Every machine-oriented command supports `--json`, `--quiet`, `--non-interactive`,
explicit project and config paths, stable schemas, and documented exit codes.

## `orc verify`

Runs the project-configured verification registry. The registry maps recognized
configuration files and lockfile-resolved tools to verification commands. The same
registry works locally and in CI (spec §9.5, MVP-8).

```sh
orc verify
orc verify --json
orc verify --quiet
```

The registry is detected from two sources:

1. **Project config** — `[[verification.commands]]` entries in `orchestraitor.toml`:

   ```toml
   [[verification.commands]]
   name = "custom-lint"
   command = "make lint"
   trigger_files = ["Makefile"]
   ```

2. **Built-in detection** — recognized configuration files mapped to default
   verification commands:

   | Config file | Command |
   |---|---|
   | `Cargo.toml` | `cargo test` |
   | `package.json` | `npm test` |
   | `pyproject.toml` | `pytest` |
   | `go.mod` | `go test ./...` |
   | `pom.xml` | `mvn test` |

   Package manager variants are detected from lockfiles (`pnpm-lock.yaml`,
   `yarn.lock`, `bun.lockb`, `uv.lock`).

Verification command execution requires an Arbitraitor sandbox (spec §6.7, §16.2).
Until the sandbox is available, `orc verify` reports the detected registry and
exits without executing untrusted commands.

## `orc policy check`

Evaluates Arbitraitor policy against a plan or recorded session and reports the
verdict in JSON. All policy evaluation is delegated to Arbitraitor; Orchestraitor
owns no security decision logic (spec §2.2, §16).

```sh
orc policy check
orc policy check --policy path/to/policy.toml
orc policy check --shadow --session <session-id>
orc policy check --json
```

The verdict is one of: `pass`, `warn`, `prompt`, `block`, `error`, `incomplete`.
A `block` verdict exits with code 4 (security block). Shadow mode reports what
would have happened without enforcement.

## `orc run --non-interactive`

Executes a task without TUI interaction. Approvals follow the configured
non-interactive policy (default: block).

```sh
orc run --non-interactive "fix the bug"
orc run --non-interactive --approval allow "fix the bug"
orc run --non-interactive --json "fix the bug"
```

The `--approval` flag controls the non-interactive approval policy:

- `block` (default) — block all operations requiring approval
- `allow` — permit non-interactive approvals

When the approval policy is `block` and a task requires approval, the command
exits with code 4 (security block).

## `orc evidence export`

Produces a privacy-preserving archive of session evidence. Secrets, prompts,
completions, tool arguments, and MCP payloads are always redacted (spec §9.17.1).

```sh
orc evidence export
orc evidence export --session <session-id>
orc evidence export --output evidence.jsonl
orc evidence export --full
orc evidence export --json
```

The export uses the tamper-evident hash-chained event store from
`orchestraitor-events`. The `--full` flag preserves payloads except fields that
are always sensitive; the default is redacted mode.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Success |
| 1 | General failure (uncategorized) |
| 2 | Configuration error |
| 3 | Verification failure |
| 4 | Security block (policy denial, approval required but blocked) |
| 5 | Infrastructure failure (daemon, transport, storage) |

Exit codes are stable and documented. CI pipelines may branch on them.
