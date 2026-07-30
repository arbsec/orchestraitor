# ADR 0005: rmcp for MCP and agent-client-protocol for ACP

**Status:** Accepted
**Date:** 2026-07-30
**Issue:** #69

## Context

Orchestraitor is a local-first coding-agent harness that integrates with
existing coding-agent harnesses (Claude Code, Codex CLI, Gemini CLI, OpenCode,
Pi, and other ACP-compatible agents) and exposes its own MCP servers and tools.
It needs stable, actively maintained Rust libraries for both the Model Context
Protocol (MCP) and the Agent Client Protocol (ACP).

Spec §10.5 assessed the available Rust libraries on 2026-07-23. Spec §24
(open questions / recommended immediate implementation decisions) records the
baseline protocol versions. Tech-stack §4.1 and §4.2 pin the concrete crate
versions.

## Decision

### MCP: `rmcp` 2.2.0

Use the **official Rust MCP SDK**, `rmcp` 2.2.0 (Jul 8, 2026), maintained at
`modelcontextprotocol/rust-sdk`, Apache-2.0. ~917k downloads/week.

Features used: `server`, `client`, `macros`, `schemars`, `auth`,
`transport-io`, `transport-streamable-http-server`,
`transport-streamable-http-client-reqwest`, `reqwest`. TLS defaults to rustls
via the reqwest feature — no `native-tls` anywhere (tech-stack §5.2).

`rmcp` provides active protocol coverage, transports, OAuth, roots, sampling,
tasks, subscriptions, and schema macros. It is the official SDK, not a
third-party reimplementation.

### ACP: `agent-client-protocol` 1.3.0

Use `agent-client-protocol` 1.3.0 (Jul 20, 2026), maintained at
`agentclientprotocol/rust-sdk` (organization moved away from the previous
`zed-industries/...` home; that path redirects), Apache-2.0. ~102k
downloads/week.

Companion crates: `agent-client-protocol-schema`, `-http`, `-rmcp` (MCP
bridge), `-tokio`, `-derive`, `-conductor`. The v1 wire stability contract
holds since v1.0 (Jun 29, 2026).

The `agent-client-protocol-rmcp` crate provides the ACP ↔ MCP bridge.

### Version pins

| Concern | Crate | Version | License |
|---|---|---|---|
| MCP server + client | `rmcp` | 2.2.0 | Apache-2.0 |
| Agent Client Protocol | `agent-client-protocol` | 1.3.0 | Apache-2.0 |
| ACP ↔ MCP bridge | `agent-client-protocol-rmcp` | 1.3.0 | Apache-2.0 |

Both are Apache-2.0, compatible with Orchestraitor's dual `MIT OR Apache-2.0`
license (tech-stack §18).

### Dependency policy

- Pin exact minor versions during the prototype (tech-stack §10.5).
- Verify the pinned release before implementation. CI runs cargo-deny + a
  custom smoke check that issues an ACP `initialize` against the bundled
  conductor stub (tech-stack §4.2).
- The Arbitraitor MCP server is wired separately — see
  [ADR 0006](0006-explicit-arbitraitor-mcp-wiring.md).

## Consequences

- Orchestraitor uses the official MCP Rust SDK, not a third-party
  reimplementation. Protocol coverage, transports, and schema macros are
  maintained upstream.
- ACP integration covers JetBrains IDEs, Zed, Gemini CLI, and the growing ACP
  agent ecosystem without inventing a new IDE-agent protocol (spec §10.5).
- The ACP ↔ MCP bridge (`agent-client-protocol-rmcp`) lets Orchestraitor
  expose MCP tools to ACP clients and vice versa.
- Version bumps to `rmcp` or `agent-client-protocol` require re-verification
  of the wire stability contract and the CI smoke check.

## Alternatives considered

- **Invent a new IDE-agent protocol:** Rejected. ACP already solves the
  IDE-agent protocol problem and has SDKs including Rust (spec §10.5).
- **Third-party MCP crate:** Rejected. `rmcp` is the official MCP Rust SDK
  with active protocol coverage; third-party crates lack the same coverage and
  upstream maintenance.
- **`rig-core` as foundational transport:** Rejected. `rig-core` includes
  agent, memory, RAG, and workflow abstractions that overlap with the product's
  main purpose (spec §10.5). Useful as a reference, not as the architectural
  core.
- **Single OpenAI-compatible DTO for all providers:** Rejected. Loses native
  features and creates translation ambiguity (spec §10.5).

## References

- `docs/spec/spec.md` §10.5 (Rust library assessment and recommendation)
- `docs/spec/spec.md` §24 (Recommended immediate implementation decisions)
- `docs/spec/tech-stack.md` §4.1 (MCP — `rmcp`)
- `docs/spec/tech-stack.md` §4.2 (ACP — `agent-client-protocol`)
- `docs/spec/tech-stack.md` §5.2 (TLS — rustls, no native-tls)
- `docs/spec/tech-stack.md` §18 (License compatibility)
- [ADR 0006](0006-explicit-arbitraitor-mcp-wiring.md) — Explicit Arbitraitor MCP wiring
