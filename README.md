# Orchestraitor

> Orchestraitor - An agent harness with trust issues.

[![Spec](https://img.shields.io/badge/spec-rev%200.14-blue)](docs/spec/spec.md)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-green)](LICENSE-MIT)

Orchestraitor is a **local-first, security-first coding-agent harness and control plane** that
combines orchestration, provider/harness adapters, contextual token optimization, and a native
developer experience — secured by [Arbitraitor](https://github.com/arbsec/arbitraitor).

Its intended design combines a complete native agent loop plus adapters for existing harnesses
(Claude Code, Codex CLI, Gemini CLI, OpenCode, Pi, and other ACP-compatible agents), enforced
runtime isolation across native and wrapped agents, static plan-bound authorization before side
effects, transactional filesystem tools, a trusted output boundary for files host tools may
later execute, an explainable context compiler, and a low-overhead native control plane for
TUI, IDE, and headless clients.

## Relationship to Arbitraitor

Arbitraitor (`arbsec/arbitraitor`) is the **exclusive security subsystem and authority** for
Orchestraitor. Every security-related primitive — policy evaluation, sandboxing, process and
filesystem containment, network and secret brokering, command/package/plugin/artifact
inspection, provenance, plan-bound approvals, output classification, promotion authorization,
and tamper-evident receipts — is implemented in Arbitraitor. Orchestraitor owns orchestration,
provider/harness adapters, context optimization, and developer experience, and **never** ships a
parallel security authority. When a security capability is missing, it is added to Arbitraitor
first (`docs/spec/spec.md` §2.2, §16).

```text
Arbitraitor     Sole security engine and policy-enforced gate for untrusted artifacts/operations
Orchestraitor   Coding-agent harness and control plane that delegates all security to Arbitraitor
```

## Status

**Pre-implementation.** The repository currently contains only its specification and governance
scaffolding. There is **no runnable software**, binary, installer, or implemented feature yet.
The API, CLI (`orc` / `orchestraitor`), daemon protocol, and configuration schema will change.

> **This software is not production-ready.** Security claims in the specification describe the
> intended design, not a shipped guarantee. Do not rely on Orchestraitor for isolation until a
> release exists and Arbitraitor reports effective controls for your platform
> (`docs/spec/spec.md` §6.7, §16.8).

## Key design principles

- **The agent is always untrusted** — model, wrapped harness, repository content, tools, MCP
  servers, skills, and generated artifacts may behave incorrectly or maliciously (spec §6.1).
- **A worktree is not a sandbox.** The trusted controller owns Git metadata (spec §6.2).
- **Approval belongs to the trusted UI**, never to agent-generated text (spec §6.4).
- **Static analysis narrows authority; it does not prove safety** (spec §6.5).
- **Arbitraitor is the sole security authority.** Missing capabilities fail closed or run in an
  explicitly-labelled non-secure mode — never a silent duplicate (spec §6.7, §16.2).
- **Transaction over mutation.** Every change is a versioned transaction: capture stage,
  normalize, verify, review a compact diff, atomically promote or roll back (spec §9.5, §9.14).
- **Opinionated by default, customizable by design, never mysterious about active config**
  (spec §9.22.11).
- **Incremental adoption.** `orc observe` → `orc wrap` → `orc connect` → native; reversible,
  with `orc disconnect` restoring prior state in under 30 seconds (spec §9.18.2, MVP-2).

## Specifications

- [`docs/spec/spec.md`](docs/spec/spec.md) — product and architecture source of truth.
- [`docs/spec/tech-stack.md`](docs/spec/tech-stack.md) — concrete crates, versions, license
  compatibility, runtime dependencies, platform support, and rejected alternatives.

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
