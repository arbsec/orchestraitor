# Introduction

> Orchestraitor - An agent harness with trust issues.

Orchestraitor is a **local-first, security-first coding-agent harness and control plane**
secured by [Arbitraitor](https://github.com/arbsec/arbitraitor). It combines a complete
native agent loop, adapters for existing coding-agent harnesses, provider-independent
context optimization, and a native developer experience — with every security primitive
delegated to Arbitraitor.

The name fuses **orchestrator** and **traitor**. It reflects both halves of the product:
Orchestraitor is a first-class coding-agent orchestrator, while its trust model assumes that
agents, wrapped harnesses, repository content, tools, MCP servers, and generated artifacts
may behave incorrectly or maliciously (spec §6.1). The word "trust" also contains "Rust," a
fitting secondary reference to the implementation language.

## What Orchestraitor is

Orchestraitor is a product, not merely a wrapper or security add-on. Its defensible purpose
is to combine capabilities that existing tools generally provide only separately (spec §1):

1. A complete native agent loop plus adapters for existing harnesses.
2. Enforced runtime isolation across native and wrapped agents.
3. Static, plan-bound authorization before side effects.
4. Transactional filesystem tools with project-aware format-on-write and safe lint fixing.
5. A trusted output boundary for files that host tools may later execute.
6. A provider-independent context compiler that reduces token usage.
7. First-class MCP, Agent Skills, AGENTS.md, ACP, and IDE interoperability.
8. Direct OpenAI, Anthropic, Gemini, and compatible endpoint support with BYOK.
9. A low-overhead native control plane for TUI, GUI, IDE, and headless clients.
10. Auditable execution, policy, context, normalization, and promotion receipts.

A coding agent should normally run in an isolated workspace and sandbox by default. The
user may explicitly weaken those guarantees, but weakening must be visible, scoped, recorded,
and never silently inferred.

## Relationship to Arbitraitor

[Arbitraitor](https://github.com/arbsec/arbitraitor) (`arbsec/arbitraitor`) is the
**exclusive security subsystem and authority** for Orchestraitor. Every security-related
primitive — policy evaluation, sandboxing, process and filesystem containment, network and
secret brokering, command/package/plugin/artifact inspection, provenance, plan-bound
approvals, output classification, promotion authorization, and tamper-evident receipts — is
implemented in Arbitraitor (spec §2.2, §16).

```text
Arbitraitor     Sole security engine and policy-enforced gate for untrusted artifacts/operations
Orchestraitor   Coding-agent harness and control plane that delegates all security to Arbitraitor
```

Orchestraitor owns orchestration, provider/harness adapters, context optimization, and
developer experience, and **never** ships a parallel security authority. When a security
capability is missing, it is added to Arbitraitor first (spec §16.2); Orchestraitor fails
closed or runs in an explicitly-labelled non-secure mode until then (spec §6.7).

The dependency direction is strict: Orchestraitor depends on Arbitraitor public crates, APIs,
capability reports, and receipts. Arbitraitor must remain independently useful and must not
depend on Orchestraitor (spec §16.3).

## Canonical naming

```text
Organization:    arbsec
Repository:       arbsec/orchestraitor
Product:          Orchestraitor
Short name:       Orc
Primary CLI:      orc
Long-form CLI:    orchestraitor
Daemon:           orcd
User config:      ~/.config/orchestraitor/
Project config:   orchestraitor.toml
Project state:    .orchestraitor/
```

`orc` is the canonical command used throughout documentation and normal shell workflows. The
long `orchestraitor` executable remains available as an explicit alias or symlink for
discoverability and to reduce ambiguity where `orc` conflicts with another installed program
(spec §1.2).

## Status

**MVP implementation in progress.** The repository contains early Rust crates for selected
MVP subsystems, including the `orcd` daemon JSON-RPC server and the `orc` CLI. There is no
tagged release or installer yet. The API, CLI, daemon protocol, and configuration schema will
change.

> **This software is not production-ready.** Security claims in the specification describe the
> intended design, not a shipped guarantee. Do not rely on Orchestraitor for isolation until a
> release exists and Arbitraitor reports effective controls for your platform (spec §6.7,
> §16.8).

## Key design principles

- **The agent is always untrusted** — model, wrapped harness, repository content, tools, MCP
  servers, skills, and generated artifacts may behave incorrectly or maliciously (spec §6.1).
- **A worktree is not a sandbox.** The trusted controller owns Git metadata (spec §6.2).
- **Approval belongs to the trusted UI**, never to agent-generated text (spec §6.4).
- **Static analysis narrows authority; it does not prove safety** (spec §6.5).
- **Arbitraitor is the sole security authority.** Missing capabilities fail closed or run in
  an explicitly-labelled non-secure mode — never a silent duplicate (spec §6.7, §16.2).
- **Transaction over mutation.** Every change is a versioned transaction: capture stage,
  normalize, verify, review a compact diff, atomically promote or roll back (spec §9.5, §9.14).
- **Opinionated by default, customizable by design, never mysterious about active config**
  (spec §9.22.11).
- **Incremental adoption.** `orc observe` → `orc wrap` → `orc connect` → native; reversible,
  with `orc disconnect` restoring prior state in under 30 seconds (spec §9.18.2, MVP-2).

## Specifications

- [`docs/spec/spec.md`](https://github.com/arbsec/orchestraitor/blob/main/docs/spec/spec.md) —
  product and architecture source of truth.
- [`docs/spec/tech-stack.md`](https://github.com/arbsec/orchestraitor/blob/main/docs/spec/tech-stack.md)
  — concrete crates, versions, license compatibility, runtime dependencies, platform support,
  and rejected alternatives.
- [`CONTRIBUTING.md`](https://github.com/arbsec/orchestraitor/blob/main/CONTRIBUTING.md) — how
  to contribute to a spec-first, security-first Rust project, including when work belongs in
  Arbitraitor instead.
- [`SECURITY.md`](https://github.com/arbsec/orchestraitor/blob/main/SECURITY.md) — report
  vulnerabilities privately; do **not** open public issues.

## License

Dual-licensed under MIT or Apache-2.0, matching Arbitraitor. All contributions are made under
the Developer Certificate of Origin.
