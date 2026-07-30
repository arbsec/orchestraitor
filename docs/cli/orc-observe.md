# `orc observe`

`orc observe -- <harness>` records a normalized event stream for the target
harness without claiming enforcement. The output always identifies as
non-protective. Use this to evaluate Orchestraitor against an existing workflow
before committing to `orc wrap` (spec §998 MVP-2).

```sh
orc observe -- claude-code
orc observe -- codex --model gpt-5
orc observe --json -- echo hello
orc observe --output ./my-observe -- make test
```

## What it records

The normalized event stream (spec §9.17) includes:

- **Session lifecycle** — start and end events with harness command and platform.
- **Process execution** — the harness process exit code, stdout/stderr byte counts.
- **Shadow policy decisions** — per-operation evaluations of what Arbitraitor's
  policy engine *would* have decided for filesystem mutations, process
  executions, network requests, and MCP tool calls. Shadow decisions do not
  affect execution.
- **Error events** — recorded when the harness fails to spawn.

Shadow policy decision outcomes (spec §9.8):

| Outcome | Meaning |
|---|---|
| `pass` | Operation would have been allowed. |
| `pass_with_constraints` | Operation would have been allowed with additional constraints. |
| `prompt` | Operation would have required user approval. |
| `block` | Operation would have been blocked. |
| `unsupported` | Operation is not supported by the current policy configuration. |
| `defer_to_stronger_sandbox` | A stronger sandbox mode is required to evaluate this operation. |

## Non-protective indicator

The output always displays a persistent "observation mode: non-protective"
indicator. No enforcement is claimed or implied. The TUI and `orc status` also
display this indicator when observation mode is active (spec §998 MVP-2).

## Output

- `--output <dir>` — directory for the recorded event stream (default:
  `.orchestraitor/observe`). The event stream is written as `events.jsonl` in
  canonical JSON Lines format with hash-chain validation (spec §9.17.1).
- `--json` — emit machine-readable JSON to stdout (one JSON object per line)
  instead of human-readable text.

## Security boundary

`orc observe` is observation-only. It does not implement sandboxing, policy
enforcement, approvals, or any security primitive — those are Arbitraitor's
exclusive responsibility (spec §2.2, §16). The shadow policy decisions are
recorded observations of what *would* have been decided, not enforcement
actions.
