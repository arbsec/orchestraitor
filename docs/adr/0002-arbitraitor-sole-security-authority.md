# ADR 0002: Arbitraitor is the sole security authority

**Status:** Accepted
**Date:** 2026-07-30
**Issue:** #69

## Context

Spec §2.2 establishes a hard architectural boundary: Arbitraitor
(`https://github.com/arbsec/arbitraitor`) is the sole implementation location
and authority for every security-related capability used by Orchestraitor.
Orchestraitor owns orchestration, provider/harness adapters, context
optimization, and developer experience — never a parallel security authority.

When a security capability is missing, Orchestraitor MUST fail closed or enter
an explicitly labelled non-secure mode (spec §6.7). It MUST NOT ship a
temporary duplicate, permissive fallback, compatibility no-op, private fork,
or hidden alternate implementation of a missing Arbitraitor control.

This decision records the concrete Arbitraitor types and crates Orchestraitor
consumes, so that integration code references real identifiers — not
conceptual names that do not compile.

## Decision

Orchestraitor implements NO security primitive. Every security evaluation and
enforcement call goes through Arbitraitor crates consumed via the pinned git
dependency (tech-stack §2.1). The authoritative integration surface
(tech-stack §2.2) is:

| Orchestraitor concern | Arbitraitor crate | Public API used |
|---|---|---|
| Sandbox effective controls (probe) | `arbitraitor-sandbox` | `EffectiveControls` struct, `ControlState` enum (`Available`/`Degraded`/`Unavailable`), `compute_effective_controls(mode, platform)`, `SandboxMode` enum (`None`/`Observe`/`Restricted`/`Disposable`), `SandboxCapabilities` struct |
| Sandbox effective controls (receipt) | `arbitraitor-exec` | `EffectiveControls`, `EffectiveControl`, `ControlStatus` enum (`Enforced`/`Partial`/`Unavailable`), `ControlProofs`, `ExecutionContextBuilder::from_operation().into_effective_controls()` |
| Policy evaluation | `arbitraitor-policy` | `PolicyEngine::load(toml)`, `merge_layers(layers, audit_override) -> LayeredPolicy`, `evaluate(findings, ctx) -> Verdict`, `evaluate_with_trace(...) -> (Verdict, PolicyTrace)`, `EvalContext`, `PolicyPrecedence`, `PolicyLayer` |
| Approval tokens | `arbitraitor-mcp` | `ApprovalTokenIssuer::new()`, `with_secret()`, `with_durable_store()`, `PlanContext::for_bash(network_isolated, policy_snapshot_digest)`, `PlanContext::for_native(...)` |
| MCP server (inspect-only default) | `arbitraitor-mcp` | `run_stdio_server()`, `build_default_server()`, `McpServer` for explicit registration of `request_approval` + `run_approved_artifact` |
| Receipts | `arbitraitor-receipt` | `Receipt`, `ReceiptBuilder`, `ApprovalInfo` (`plan_digest`, `artifact_digest`, `expiry`, `nonce`, `bound_capabilities`), `canonical_bytes()`, `sign_receipt()`, `verify_receipt()`, `to_intoto_statement()`, `redact_url()` |
| Wrapper-plugin plan classification | `arbitraitor-plugin-api` | `OperationPlan`, `PlannedOperation` enum, `PluginTrustClass` enum (`BuiltIn`/`FirstParty`/`CommunityReviewed`/`CommunityUnreviewed`), `CapabilitySet`, `Plugin` trait hierarchy |

### Conceptual names are prohibited

The following conceptual names appear in older spec revisions but **do not
exist** in current Arbitraitor code. Code referencing them will not compile:

| Conceptual name (deprecated) | Real type |
|---|---|
| `EffectiveSandboxControls` | `arbitraitor_sandbox::EffectiveControls` (probe) and `arbitraitor_exec::EffectiveControls` (receipt) — two different structs with different shapes |
| `ActionPlan` | `arbitraitor_plugin_api::OperationPlan`, `arbitraitor_model::operation::OperationPlan`, or `arbitraitor_mcp::PlanContext` depending on the call site |
| `ApprovalToken` | `arbitraitor_mcp::ApprovalTokenIssuer` (issuer), opaque `String` token (`v2.<payload_hex>.<signature_hex>`), `arbitraitor_receipt::ApprovalInfo` (receipt record) |

### Fail-closed behavior

When an Arbitraitor capability is absent, unsupported, stale, or incompatible,
Orchestraitor MUST (spec §6.7):

1. block the protected operation by default;
2. identify the missing Arbitraitor capability;
3. offer only an explicit, visibly weakened mode where policy permits;
4. record the degradation in the session and receipt;
5. direct implementation work to `arbsec/arbitraitor`, not to a parallel
   Orchestraitor control.

The `orcd` daemon's `health` JSON-RPC method reports the Arbitraitor capability
report from the startup probe (spec §6.7, §16.7) and reports `fail_closed`
when any required sandbox control is unavailable on the current platform.

## Consequences

- Orchestraitor's security posture is entirely derived from Arbitraitor's
  effective controls, probed at runtime — never from configuration that was
  requested but not verified.
- A missing Arbitraitor capability is an upstream prerequisite (tech-stack
  §2.4), not a parallel Orchestraitor implementation. Orchestraitor issues may
  track integration work but must link to the canonical Arbitraitor issue.
- The `security` domain agent (ADR 0004) is analysis-only; it never substitutes
  for an absent Arbitraitor capability.
- Integration code referencing conceptual names (`EffectiveSandboxControls`,
  `ActionPlan`, `ApprovalToken`) is a compile error by design — the type system
  enforces the boundary.

## Alternatives considered

- **Parallel Orchestraitor security layer:** Rejected. Violates spec §2.2.
  Creates a second authority that can drift, weaken, or contradict Arbitraitor.
- **Permissive fallback when Arbitraitor is unavailable:** Rejected. Silent
  weakening is the exact failure mode spec §6.7 prohibits. Fail closed or
  explicitly label the non-secure mode.
- **Conceptual type aliases:** Rejected. Aliases for `EffectiveSandboxControls`
  / `ActionPlan` / `ApprovalToken` would compile but hide that the real types
  have different shapes at different call sites. Real identifiers are mandatory.

## References

- `docs/spec/spec.md` §2.2 (Security ownership invariant)
- `docs/spec/spec.md` §6.7 (Arbitraitor is the sole security authority)
- `docs/spec/spec.md` §16 (Arbitraitor integration and ownership plan)
- `docs/spec/tech-stack.md` §2.1 (Hard constraint: no published crates)
- `docs/spec/tech-stack.md` §2.2 (Arbitraitor integration surface used by Orchestraitor)
- `docs/spec/tech-stack.md` §2.4 (Upstream Arbitraitor prerequisites)
- Arbitraitor [ADR 0007](https://github.com/arbsec/arbitraitor/blob/main/docs/adr/0007-assurance-levels-model.md) — Assurance levels model
- Arbitraitor [ADR 0013](https://github.com/arbsec/arbitraitor/blob/main/docs/adr/0013-plan-bound-approval-capability.md) — Plan-bound approval capability model
