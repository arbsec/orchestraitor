# Architecture Overview

Orchestraitor is a Rust workspace secured by [Arbitraitor](https://github.com/arbsec/arbitraitor).
The trusted control plane owns orchestration, provider/harness adapters, context compilation,
and developer experience. Arbitraitor owns every security decision — sandboxing, policy,
approvals, provenance, output promotion, and receipts (spec §2.2, §16).

## Process topology

The recommended starting topology (spec §17.2.1, subject to prototype validation) is a durable
supervisor with thin clients:

```text
CLI / TUI / GUI / IDE plugin        # thin clients — crash-safe; never the runtime owner
    |
    v
orcd (durable supervisor)            # scheduler, config resolver, event owner
  +-- mcp-gateway (supervised proc)  # mediates MCP protocol; resolves project-specific server sets
  +-- worker process [agent A]       # separate process per active agent/subagent attempt
  +-- worker process [agent B]       # isolated; failure does not terminate sibling work
  +-- mcp-server [local stdio #1]    # Arbitraitor-controlled sandboxed process
  +-- mcp-server [local stdio #2]    # Arbitraitor-controlled sandboxed process
```

### Daemon as durable supervisor

`orcd` is the **durable supervisor, scheduler, configuration resolver, and event owner**
(spec §17.2.1). It survives client and gateway crashes. UI or gateway crashes must not
terminate durable work; `orcd` supervises and reaps, and crashes are recovered per the task
and session lifecycle (spec §9.24).

The `orcd` binary runs a JSON-RPC server over a Unix-domain socket using Tokio's
current-thread runtime. It currently exposes:

- `initialize` — protocol version negotiation.
- `health` — daemon status plus the Arbitraitor capability report from the startup probe
  (spec §6.7, §16.7); reports `fail_closed` when any required sandbox control is unavailable
  on the current platform.
- `shutdown` — graceful shutdown within the five-second budget.

By default, `orcd` listens at the first positional path argument, then the
`ORCHESTRAITOR_DAEMON_SOCKET` environment variable, then a temporary default path. `SIGTERM`
triggers graceful shutdown within the five-second daemon budget (tech-stack §10).

### TUI as reference client

The TUI (`orchestraitor-tui`, built on `ratatui`) is a **thin client**. CLI, TUI, GUI, and
IDE plugins connect to `orcd` and must not own runtime state (spec §17.2.9). If the UI
crashes, work continues. The TUI is the reference client for the control plane; it presents
plans, findings, approvals, receipts, cost, and routing decisions, but the durable state
lives in `orcd` and the event store.

### MCP gateway

The MCP gateway is a **separate process supervised by `orcd`** (spec §17.2.1). It mediates
MCP protocol operations between clients (agents, IDEs) and the resolved set of local and
remote MCP servers. It exposes one registration surface to clients while resolving
project-specific server sets internally:

```sh
# stdio transport (for agents that spawn a subprocess)
orc mcp stdio --project auto   # auto-detects project from cwd

# HTTP transport (for IDEs and daemons)
http://127.0.0.1:<port>/mcp/projects/<project-id>/tools
```

The gateway routes and namespaces; it is **not a security boundary** (spec §17.2.5). A proxy
may block MCP calls by refusing to route a tool invocation, but it cannot mediate filesystem
syscalls made directly by a sandboxed MCP server process. Filesystem containment is
Arbitraitor's exclusive domain (§9.4.2, §16.4). Each local MCP server runs as a separately
sandboxed Arbitraitor-controlled process.

### Project isolation

Servers, tools, credentials, indexes, and state must not leak between projects (spec
§17.2.3). Each project's MCP server set, agent context, workspace, cost ledger, and event
store are isolated. The gateway resolves the project from the connection's project-id before
listing tools and refuses cross-project tool calls.

## Workspace model

Orchestraitor creates an isolated workspace for every new session by default (spec §9.4,
§3.1). A worktree is not a sandbox (spec §6.2): the trusted controller owns Git metadata, and
the shared Git object database stays outside the worker trust boundary. Workspace backends
(spec §9.4.2, §9.32.2) include projected VFS (maximum mediation), native overlay
(kernel-native, coarser), and materialized workspace (simplest, no per-op mediation) — all
implemented in Arbitraitor.

Every mutation is a versioned transaction (spec §9.5, §9.14): capture base generation, stage
changes, detect side effects, normalize, verify, review a compact diff, and atomically
promote or roll back. The trusted checkout is never corrupted by partial failure, concurrent
IDE edits, or background processes.

## Crate layout

The workspace is organized into roughly twenty crates (spec §16.6). Orchestraitor-owned
crates use the `orchestraitor-*` prefix; the shorter `orc-*` prefix is reserved for private
workspace modules where collision is controlled. The current repository layout:

| Crate | Role |
|---|---|
| `orchestraitor-core` | Shared primitives, types, and config foundations. |
| `orchestraitor-daemon` (`orcd`) | Durable supervisor: JSON-RPC server, capability probe, supervision, governance, event store. |
| `orchestraitor-cli` (`orc`) | Local control-plane CLI: init, config, models catalog. |
| `orchestraitor-arbitraitor-client` | Thin client re-exporting Arbitraitor crates (`arbitraitor-sandbox`, `arbitraitor-policy`, `arbitraitor-mcp`, `arbitraitor-receipt`, …) and project-owned wrappers. |
| `orchestraitor-model` | Domain model: digests, findings, verdicts. |
| `orchestraitor-workspace-hack` | Workspace dependency deduplication. |
| `orchestraitor-context` | Context compiler: selection, token budgeting, provenance (spec §9.15). |
| `orchestraitor-events` | Event and receipt store (spec §9.17). |
| `orchestraitor-mcp` | MCP gateway integration and tool exposure (spec §17.2). |
| `orchestraitor-adapter-api` / `-adapter-host` | Agent and harness adapter traits + host (spec §10). |
| `orchestraitor-provider-api` / `-provider-proxy` / `-provider-meta` / `-provider-neuralwatt` | Provider transport traits, OpenAI-compatible proxy, metadata, and the GLM-5.2 BYOK adapter (spec §10.3). |
| `orchestraitor-agent-catalog` | Agent/domain registry, routing, detection (spec §9.19, §9.21). |
| `orchestraitor-cost-ledger` | Per-agent/domain/provider/subscription cost attribution (spec §9.19.4). |
| `orchestraitor-lifecycle` | Task and session lifecycle, retry, recovery (spec §9.24, §9.26). |
| `orchestraitor-tui` | Reference `ratatui` thin client. |

Spec §16.6 also proposes integration crates for JetBrains and VS Code, and adapters for
Claude, Codex, Gemini, OpenCode, and Pi. Do not create Orchestraitor crates named `sandbox`,
`policy`, `network-broker`, `secret-broker`, `approval`, or `security-receipt` if they would
own security logic — a narrowly scoped client or presentation crate is acceptable only when
its name and API make the delegation to Arbitraitor unambiguous (spec §16.6).

## Security boundary

The ownership boundary is strict (spec §2.2, §16.4):

```text
Orchestraitor owns:                      Arbitraitor owns:
  agent loops and session orchestration     policy evaluation and security decisions
  provider and harness adapters             sandboxing and effective-control verification
  CLI, TUI, GUI, IDE, MCP, ACP, proxy      process, filesystem, network, secret enforcement
  context selection and token optimization command, package, plugin, artifact inspection
  project config discovery and migration    provenance, signatures, trust roots
  format-on-write workflow and diagnostics plan binding and approval validation
  presentation of plans/findings/receipts   output classification and promotion authorization
                                            tamper-evident security receipts
                                            fail-closed behavior and capability matrices
```

Orchestraitor may translate a user or agent action into an Arbitraitor request and present the
result, but it may not become an alternative security authority. Even when integration code
runs inside `orcd`, the enforcement algorithm and security decision must originate from
Arbitraitor-owned code (spec §2.2). See
[Arbitraitor Integration](../reference/arbitraitor-integration.md) for the capability probes,
MCP wiring, and fail-closed behavior.

See the [full specification](https://github.com/arbsec/orchestraitor/blob/main/docs/spec/spec.md)
for the authoritative design.
