# Orchestraitor

> Orchestraitor - An agent harness with trust issues.

[![Code](https://github.com/arbsec/orchestraitor/actions/workflows/code.yml/badge.svg)](https://github.com/arbsec/orchestraitor/actions/workflows/code.yml)
[![Security](https://github.com/arbsec/orchestraitor/actions/workflows/security.yml/badge.svg)](https://github.com/arbsec/orchestraitor/actions/workflows/security.yml)

Orchestraitor is a **local-first, security-first coding-agent harness and control plane** that
combines orchestration, provider/harness adapters, contextual token optimization, and a native
developer experience — secured by [Arbitraitor](https://github.com/arbsec/arbitraitor).

Its intended design combines a complete native agent loop plus adapters for existing harnesses
(Claude Code, Codex CLI, Gemini CLI, OpenCode, Pi, and other ACP-compatible agents), enforced
runtime isolation across native and wrapped agents, static plan-bound authorization before side
effects, transactional filesystem tools, a trusted output boundary for files host tools may
later execute, an explainable context compiler, and a low-overhead native control plane for
TUI, IDE, and headless clients.

Its first product axis is a bounded, self-improving delivery loop: work items live on a kanban
board, a fresh manager session selects the next eligible task, a worker implements it in an
isolated workspace and produces a structured change set (PR delivery lands with the campaign
delivery lane), adversarial review converges on the result, and
merges of security-sensitive changes are human-gated. Orchestraitor's own backlog is the loop's
first and continuous workload (self-hosting) (spec `00-overview.md` §1, §2.3, §3.1).

## The delivery loop

The loop is a fixed cycle, not a free-running swarm:

```text
board poll → manager selection → worker implementation → structured change set
           → adversarial review → human-gated merge
```

- **Board poll** — the runner reads the reconciled GitHub Projects v2 board through the
  [`orchestraitor-board-contract`](crates/orchestraitor-board-contract) provider contract
  (write-through, board-wins cache).
- **Manager selection** — one campaign pass applies the P0-first epic-focus rule, selects at
  most one eligible task, and persists exactly one append-only decision record.
- **Worker** — a headless bootstrap worker implements the task in a path-confined worktree
  with exactly four tools, all security primitives mediated by Arbitraitor, and produces a
  structured change set (the pull-request sink lands with the campaign delivery lane).
- **Adversarial review** — independent review converges on the result before merge.
- **Human-gated merge** — security-sensitive changes always require human review.

The loop will be bounded by an explicit guard set (issue #310): attempts, re-plans, worker
timeout, concurrency cap, supervisor stall kill, exponential backoff, a daily spend soft cap,
and a run budget. Guard-weakening configuration is rejected fail-closed. Run state is durable
(`loop.db`, one row per supervised run, updated with heartbeat liveness and a terminal
status; the decision records in `campaign.db` are the append-only audit trail), and a
single-instance lock ensures only one loop invocation runs at a time. These loop mechanics
land with #434 — they are not shipped yet.

See [`docs/cli/orc-campaign.md`](docs/cli/orc-campaign.md) for the manager selection pass;
the cron-shaped `orc loop` runner and its `docs/cli/orc-loop.md` reference land with the
bootstrap-loop PR (#434).

## Relationship to Arbitraitor

Arbitraitor (`arbsec/arbitraitor`) is the **exclusive security subsystem and authority** for
Orchestraitor. Every security-related primitive — policy evaluation, sandboxing, process and
filesystem containment, network and secret brokering, command/package/plugin/artifact
inspection, provenance, plan-bound approvals, output classification, promotion authorization,
and tamper-evident receipts — is implemented in Arbitraitor. Orchestraitor owns orchestration,
provider/harness adapters, context optimization, and developer experience, and **never** ships a
parallel security authority. When a security capability is missing, it is added to Arbitraitor
first (spec `40-arbitraitor-integration.md` §2.2, §16).

```text
Arbitraitor     Sole security engine and policy-enforced gate for untrusted artifacts/operations
Orchestraitor   Coding-agent harness and control plane that delegates all security to Arbitraitor
```

## Status

**MVP implementation in progress.** The repository now contains early Rust crates for selected
MVP subsystems, including the `orcd` daemon JSON-RPC server. There is no tagged release or
installer yet. The API, CLI (`orc` / `orchestraitor`), daemon protocol, and configuration
schema will change.

### Early daemon surface

The `orcd` binary runs a JSON-RPC server over a Unix-domain socket using Tokio's
current-thread runtime. It currently exposes:

- `initialize` — protocol version negotiation
- `health` — daemon status plus the Arbitraitor capability report from the
  startup probe (spec `40-arbitraitor-integration.md` §6.7, §16.7); reports `fail_closed` when any required
  sandbox control is unavailable on the current platform
- `shutdown` — graceful shutdown within the five-second budget

By default, `orcd` listens at the first positional path argument, then
`ORCHESTRAITOR_DAEMON_SOCKET`, then a temporary default path. `SIGTERM` triggers graceful
shutdown within the five-second daemon budget from `docs/spec/tech-stack.md` §10.

### Now running

The loop's first concrete surfaces ship today:

- [`orc board`](docs/cli/orc-board.md) — ready-queue read and verified Status writes against
  the shared GitHub Projects v2 board.
- [`orc worker`](docs/cli/orc-worker.md) — headless one-shot bootstrap worker with exactly
  four tools, all bash mediated by Arbitraitor (network-denied execution policy, fail-closed
  capability preflight).
- [`orc campaign run --once`](docs/cli/orc-campaign.md) — one manager decision per pass:
  P0-first selection, exactly one append-only decision record.
- [`orc routing resolve`](docs/cli/orc-routing.md) — role routing over the six built-in
  orchestration roles, custom roles, and the default-off `DecisionProvider` fixture.
- [`orc config`](docs/cli/orc-init.md) — configuration inspection, validation, diff, and
  forward-only migration.
- [`orc github mint-token`](docs/cli/orc-github.md) — GitHub App installation-token minting
  (non-secret metadata only).
- [`orc board query`](docs/cli/orc-board.md) — the read-only `board.query` coordinator
  decision tool (#458): typed filter search plus a transitive blocked-graph walk with cycle
  detection. In this slice it reads a deterministic in-memory fixture board; the live
  sqlite/GitHub provider wiring is a follow-up (#318 split).
- The `decision.record` coordinator decision tool (#334) — persists one append-only,
  replayable §9.35 decision record into the campaign store, with typed write validation and
  fail-closed secret refusal; documented in
  [`docs/cli/orc-decision-record.md`](docs/cli/orc-decision-record.md).
- [`orc board guarded-move`](docs/cli/orc-board.md) — the `board.move` coordinator decision
  tool (#333): guarded status-class transitions — workflow-policy validated, lease-checked,
  reconcile-visible; refusals are typed and leave the board unchanged. Fixture board in this
  slice (#318 split).
- The `orchestraitor-board-contract` crate — the `BoardProvider` contract with a
  write-through, board-wins read cache (spec §9.43).

The cron-shaped `orc loop` runner and the always-on `orcd watch` daemon
(§9.36 thin slice, #503) ship today — see
[docs/cli/orcd-watch.md](docs/cli/orcd-watch.md).

> **This software is not production-ready.** Security claims in the specification describe the
> intended design, not a shipped guarantee. Do not rely on Orchestraitor for isolation until a
> release exists and Arbitraitor reports effective controls for your platform
> (spec `40-arbitraitor-integration.md` §6.7, §16.8).

## Key design principles

- **The agent is always untrusted** — model, wrapped harness, repository content, tools, MCP
  servers, skills, and generated artifacts may behave incorrectly or maliciously (spec `40-arbitraitor-integration.md` §6.1).
- **A worktree is not a sandbox.** The trusted controller owns Git metadata (spec `40-arbitraitor-integration.md` §6.2).
- **Approval belongs to the trusted UI**, never to agent-generated text (spec `40-arbitraitor-integration.md` §6.4).
- **Static analysis narrows authority; it does not prove safety** (spec `40-arbitraitor-integration.md` §6.5).
- **Arbitraitor is the sole security authority.** Missing capabilities fail closed or run in an
  explicitly-labelled non-secure mode — never a silent duplicate (spec `40-arbitraitor-integration.md` §6.7, §16.2).
- **Transaction over mutation.** Every change is a versioned transaction: capture stage,
  normalize, verify, review a compact diff, atomically promote or roll back (spec `20-harness-worker.md` §9.5, `40-arbitraitor-integration.md` §9.14).
- **Opinionated by default, customizable by design, never mysterious about active config**
  (spec `50-contracts-data.md` §9.22.11).
- **Incremental adoption.** `orc observe` → `orc wrap` → `orc connect` → native; reversible,
  with `orc disconnect` restoring prior state in under 30 seconds (spec `20-harness-worker.md` §9.18.2, `60-milestones.md` MVP-2).

## Specifications

- [`docs/spec/00-overview.md`](docs/spec/00-overview.md) — product and architecture source
  of truth for the orchestrator-first document set;
  [`spec.md`](docs/spec/spec.md) is the compatibility index for legacy `§N` references.
- [`docs/spec/tech-stack.md`](docs/spec/tech-stack.md) — concrete crates, versions, license
  compatibility, runtime dependencies, platform support, and rejected alternatives.

## CLI

- [`orc init`](docs/cli/orc-init.md) — deterministic local project detection that writes a
  proposed `.orchestraitor/orchestraitor.toml`; `--dry-run` writes nothing.
- [`orc board`](docs/cli/orc-board.md) — ready-queue read, verified Status write, and the
  board coordinator decision tools (`board.query` typed filter search + transitive
  blocked-graph walk; `board.move` guarded transitions with typed refusals,
  §9.39/§9.40) against the shared GitHub Projects v2 board
  (spec `10-orchestrator.md` §9.43, §9.40).
- [`orc worker`](docs/cli/orc-worker.md) — headless one-shot bootstrap worker: runs one
  leaf task through the bounded mini-agent loop with exactly four tools (file read,
  local content search, Arbitraitor-mediated bash, path-confined worktree write) and
  prints a structured result (spec `10-orchestrator.md` §9.38, `60-milestones.md` MVP-6).
- [`orc campaign`](docs/cli/orc-campaign.md) — one-shot campaign pass: reads the
  reconciled board, applies the P0-first epic-focus rule, selects at most one eligible
  task, persists exactly one append-only decision record, and spawns the worker via the
  daemon-less direct path (spec `10-orchestrator.md` §9.35).
- [`orc loop`](docs/cli/orc-loop.md) — the cron-shaped foreground bootstrap loop: poll
  the board, run one campaign pass, supervise in-flight workers, repeat — under the
  minimal guard set (concurrency cap, stall kill, worker timeout, backoff, spend soft
  cap, run budget; spec `10-orchestrator.md` §9.36 thin slice, issue #314).
- [`orc simplify`](docs/cli/orc-simplify.md) — the rule-driven pre-landing simplify
  pass: cargo clippy / cargo fmt / rumdl / cargo-machete rendered as typed §9.5
  suggestions (Format / Safe fix / Semantic), fail-open, never a blocker.
- [Bootstrap Loop Quickstart](book/src/getting-started/loop-quickstart.md) — end-to-end
  owner walkthrough for the cron-shaped `orc loop` bootstrap runner ([#434][]): what to
  configure before the first invocation, what a pass does, how to read `loop.db` and the
  summary JSON, and the fixed bootstrap guard set.

[#434]: https://github.com/arbsec/orchestraitor/pull/434

## CLI configuration surface

The `orc` binary also exposes the configuration inspection and migration commands required by
spec `50-contracts-data.md` §9.22.3 and §9.22.8:

```sh
orc config get <key>
orc config explain <key>
orc config set <key> <value> [--layer=project|user|org|dir]
orc config unset <key> [--layer=project|user|org|dir]
orc config validate
orc config diff [--layer=project|user|org|dir] [--json]
orc config migrate
orc models refresh
orc models rollback
orc github mint-token
orc routing resolve --role <id> [--json]
orc campaign run --once [--json]
orc board query [--blocked-by <item-id> | [--item-type <type>] [--status <name>] [--field <name>=<option>]...] [--json]
orc board guarded-move <item-id> --status "<Status option name>" --session <session-label> [--json]
orc loop --max-cycles N [--json]
```

The last line lands with #434; `orc board query` shipped with #458 (fixture board;
`--blocked-by` selects blocked-graph mode, the filter flags select filter mode) and
`orc board guarded-move` with #333 (fixture board; guarded `board.move`).

`orc config explain` reports the resolved value, source layer, source file, inherited state,
and profile contribution placeholder. `orc config validate` rejects ambiguous same-layer
conflicts (two shards under the same layer both defining the same key) and reports unknown
keys. `orc config migrate` is forward-only, writes a `.bak.*` backup, and uses `toml_edit` so
existing comments survive migration. `orc models refresh` forces an immediate models.dev
catalog fetch into the local cache; `orc models rollback` returns to the previous cached
snapshot without deleting manually configured models. `orc routing resolve` resolves one of
the six built-in orchestration roles (`explore`, `research`, `plan`, `implement`, `review`,
`verify`) to its configured `(provider, model)` pair and persists the routing decision
record — see [docs/cli/orc-routing.md](docs/cli/orc-routing.md). Setting the
default-off `routing.provider = "fixture"` config key consults the
deterministic fixture `DecisionProvider` first (typed structured output with
calibrated confidence); the heuristic table stays the fallback chain whenever
the provider errors or is unavailable (spec §9.45).

## Bootstrap worker sandbox mediation

The bootstrap mini-worker's execution path is gated behind Arbitraitor (issue #311; the
bootstrap loop itself wires the worker in via #310). Worker spawn runs a capability
preflight —
[`arbitraitor_sandbox::compute_effective_controls(SandboxMode::Restricted, platform)`](docs/spec/40-arbitraitor-integration.md#96-arbitraitor-sandbox-integration)
— records the controls matrix + verdict (`Allowed`/`Refused`) into the run
state, and refuses to start when any required control is unavailable (typed
error naming the missing controls) or when the platform is not Linux
(ADR-0024 fail closed; no non-secure mode on this path). Bash runs through
`arbitraitor_exec::ExecutionContextBuilder` under an explicit, network-denied
`ExecutionPolicy`; no direct `std::process` spawn exists on the worker path.
See [docs/sandbox-mediation.md](docs/sandbox-mediation.md) for the flow,
fail-closed semantics, and the pinned-API mapping.

## GitHub App service identity

All agent-driven GitHub operations authenticate as the org-owned `arbsec-agent`
GitHub App (installation access tokens minted from the App private key; ~1h
expiry; no long-lived PAT). `orc github mint-token` exercises the minting path
and prints only non-secret metadata. See
[docs/cli/orc-github.md](docs/cli/orc-github.md) for configuration and
fail-closed behavior (spec `10-orchestrator.md` §9.25.2).

## Contributing and security

- [CONTRIBUTING.md](CONTRIBUTING.md) — how to contribute to a spec-first, security-first Rust
  project, including when work belongs in Arbitraitor instead.
- [SECURITY.md](SECURITY.md) — report vulnerabilities privately; do **not** open public issues.
- [AGENTS.md](AGENTS.md) — always-active agent and contributor rule set.
- [.agents/project/orchestraitor-workflow.md](.agents/project/orchestraitor-workflow.md) —
  MVP scheduling, review domains, documentation, and merge invariants.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), matching Arbitraitor.
All contributions are made under the Developer Certificate of Origin.
