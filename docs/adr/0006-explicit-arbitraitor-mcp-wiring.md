# ADR 0006: Explicit Arbitraitor MCP wiring for Approve and Execute

**Status:** Accepted
**Date:** 2026-07-30
**Issue:** #69

## Context

Spec §9.9 and tech-stack §2.3 define a security-critical wiring requirement
for the Arbitraitor MCP server. The default Arbitraitor MCP stdio server is
**inspect-only**. The Approve (`request_approval`) and Execute
(`run_approved_artifact`) capabilities are NOT registered by default — they
require explicit construction with injected dependencies.

Treating the default stdio server as providing approval or execution
capabilities is a **security-critical bug** (spec §9.9, tech-stack §2.3). An
agent that believes it can request approval or execute approved artifacts
through the default server would be operating against a server that silently
lacks those tools — or worse, against a future server that registers them
without the required injected dependencies.

## Decision

### Default server is inspect-only

The default Arbitraitor MCP stdio server, constructed via
`arbitraitor_mcp::build_default_server()` and started by
`arbitraitor_mcp::run_stdio_server()`, registers ONLY inspect-class tools:

| Tool | Class | Registered by default? |
|---|---|---|
| `inspect_url` | Inspect | Yes |
| `fetch_artifact` | Inspect | Yes |
| `scan_artifact` | Inspect | Yes |
| `query_receipt` | Inspect | Yes |
| `explain_verdict` | Inspect | Yes |
| `request_approval` | Approve | **No** |
| `run_approved_artifact` | Execute | **No** |

### Explicit `McpServer` construction for Approve + Execute

To enable the Approve (`request_approval`) and Execute
(`run_approved_artifact`) capabilities, Orchestraitor MUST construct an
`arbitraitor_mcp::McpServer` instance with explicitly injected dependencies:

| Dependency | Type | Purpose |
|---|---|---|
| Approval token issuer | `arbitraitor_mcp::ApprovalTokenIssuer` | Constructed via `ApprovalTokenIssuer::new()`, configured with `with_secret()` and/or `with_durable_store()` |
| Artifact lookup | `ArtifactLookup` | Resolves approved artifacts by digest |
| Receipt lookup | `ReceiptLookup` | Retrieves receipts for verification |
| Plan context | `arbitraitor_mcp::PlanContext` | ADR-0013 binding context; `PlanContext::for_bash(network_isolated, policy_snapshot_digest)` or `PlanContext::for_native(...)` |

### Vocabulary (real identifiers, not conceptual names)

The approval type was previously shown as `ApprovalToken`. That struct **does
not exist** in current Arbitraitor code (spec §9.9, tech-stack §2.2). The
actual approval surface is:

- `arbitraitor_mcp::ApprovalTokenIssuer` — public issuer (`new()`,
  `with_secret(...)`, `with_durable_store(...)`, `issue()`, `validate()`). The
  issued token is an opaque `String` of the form
  `v2.<payload_hex>.<signature_hex>` (HMAC-SHA256, schema_version 3, default
  5-minute lifetime).
- `arbitraitor_mcp::ApprovalTokenPayload` — private; Orchestraitor does not
  construct or read it directly.
- `arbitraitor_mcp::PlanContext` — public ADR-0013 binding context
  (`for_bash(network_isolated, policy_snapshot_digest)`, `for_native(...)`,
  …). Orchestraitor assembles this.
- `arbitraitor_receipt::ApprovalInfo` — public receipt-recorded payload
  (`plan_digest`, `artifact_digest`, `expiry`, `nonce`, `bound_capabilities`,
  `override_reason`, `override_scope`, `exit_status`).

### Phase 0 implementation task

This wiring is a Phase 0 implementation task (tech-stack §2.3). The plan in
`.omo/plans/orchestraitor-mvp-bootstrap.md` tracks it. Until the explicit
`McpServer` construction is in place, Orchestraitor MUST NOT present approval
or execution capabilities to agents — the absence is fail-closed, not silent.

### Agent capability separation

Per Arbitraitor [ADR 0013](https://github.com/arbsec/arbitraitor/blob/main/docs/adr/0013-plan-bound-approval-capability.md),
three separate capabilities are exposed:

| Capability | What it does | Who uses it |
|---|---|---|
| `inspect` | Retrieve, scan, report findings. No release. | Agent |
| `request_approval` | Submit plan for human review. Cannot self-approve. | Agent |
| `execute_approved` | Execute using a pre-issued approval token. | Agent or CI |

The agent that requests inspection or execution **cannot** also satisfy human
approval through the same capability. Approval is rendered by the core-owned
UI or another authenticated channel — never through agent-provided text.

## Consequences

- The default Arbitraitor MCP server is safe to expose to agents for
  inspection without risk of accidental approval or execution.
- Enabling Approve + Execute is an explicit, auditable construction step with
  injected dependencies — not a configuration flag or a default-on behavior.
- Orchestraitor integration code references real Arbitraitor types
  (`ApprovalTokenIssuer`, `PlanContext`, `McpServer`, `ApprovalInfo`), not the
  conceptual `ApprovalToken` name that does not compile.
- Until the explicit wiring lands, Orchestraitor fails closed: no approval or
  execution capabilities are presented to agents.

## Alternatives considered

- **Default server with all capabilities enabled:** Rejected. A server that
  silently provides approval and execution without injected dependencies
  (`ApprovalTokenIssuer`, `ArtifactLookup`, `ReceiptLookup`, `PlanContext`)
  would have no token secret, no artifact resolution, no receipt verification,
  and no plan binding — a security-critical bug.
- **Configuration flag to enable Approve + Execute on the default server:**
  Rejected. A flag does not inject the required dependencies. The capabilities
  need a real `ApprovalTokenIssuer` with a secret and durable store, a real
  `PlanContext` bound to the operation, and real lookup types.
- **Conceptual `ApprovalToken` type alias:** Rejected. No struct of this name
  exists in Arbitraitor. Code referencing `ApprovalToken` will not compile.
  The issued token is an opaque `String`; the issuer is `ApprovalTokenIssuer`;
  the receipt record is `ApprovalInfo`.

## References

- `docs/spec/spec.md` §9.9 (Arbitraitor approval integration)
- `docs/spec/spec.md` §2.2 (Security ownership invariant)
- `docs/spec/spec.md` §6.7 (Arbitraitor is the sole security authority)
- `docs/spec/tech-stack.md` §2.2 (Arbitraitor integration surface — `arbitraitor-mcp`)
- `docs/spec/tech-stack.md` §2.3 (Mandatory Arbitraitor MCP wiring)
- Arbitraitor [ADR 0013](https://github.com/arbsec/arbitraitor/blob/main/docs/adr/0013-plan-bound-approval-capability.md) — Plan-bound approval capability model
- [ADR 0002](0002-arbitraitor-sole-security-authority.md) — Arbitraitor is the sole security authority
