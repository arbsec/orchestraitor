# Arbitraitor Integration

[Arbitraitor](https://github.com/arbsec/arbitraitor) (`arbsec/arbitraitor`) is the sole
security subsystem and authority for Orchestraitor. This page describes the ownership
boundary, the dependency model, the capability probes, the mandatory MCP wiring, and the
fail-closed behavior that governs every integration.

See [spec §16](https://github.com/arbsec/orchestraitor/blob/main/docs/spec/spec.md) and
[tech-stack §2](https://github.com/arbsec/orchestraitor/blob/main/docs/spec/tech-stack.md) for
the authoritative design.

## Ownership boundary (spec §2.2, §16.1)

Arbitraitor is not merely a collection of reusable crates or an optional backend. It is
Orchestraitor's complete security subsystem. This is a hard architectural boundary:

- Orchestraitor **must not** implement an independent sandbox, policy engine, security
  scanner, provenance verifier, approval validator, network enforcement layer, secret
  enforcement layer, promotion authorization mechanism, security receipt format, or
  security-sensitive command classifier.
- Orchestraitor **must** call Arbitraitor crates, stable APIs, or an Arbitraitor service
  boundary for security evaluation and enforcement.
- If a required security feature is missing, it **must** be added to the Arbitraitor GitHub
  project first, including its threat model, tests, documentation, capability reporting, and
  fail-closed behavior (spec §16.2). Orchestraitor must not ship a temporary duplicate,
  permissive fallback, compatibility no-op, private fork, or hidden alternate implementation.
- Arbitraitor must remain independently useful and must not depend on Orchestraitor (spec
  §16.3). The dependency direction is strict: `Orchestraitor -> Arbitraitor`; never the
  reverse.

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
Arbitraitor-owned code (spec §2.2).

## Dependency model (tech-stack §2.1)

As of 2026-07-23, all Arbitraitor workspace crates set `publish = false`. Orchestraitor must
not depend on Arbitraitor via crates.io. Two integration paths:

1. **Git dependency pinned to a specific commit** (recommended for tagged releases).
2. **Local path override** via Cargo `[patch]` (for development against `../arbitraitor`).

The `orchestraitor-arbitraitor-client` crate re-exports the Arbitraitor crates Orchestraitor
consumes and adds project-owned wrappers. It depends on the real Arbitraitor crates:

```toml
arbitraitor-core       = { workspace = true }
arbitraitor-exec       = { workspace = true }
arbitraitor-mcp        = { workspace = true }
arbitraitor-model      = { workspace = true }
arbitraitor-plugin-api = { workspace = true }
arbitraitor-policy     = { workspace = true }
arbitraitor-receipt    = { workspace = true }
arbitraitor-sandbox    = { workspace = true }
```

The Arbitraitor git revision is pinned to the latest `main` HEAD at implementation start time
and updated weekly. Exact-rev pinning is the only safe option because Arbitraitor's API
surface is pre-1.0 and may shift commit-to-commit.

## Real Arbitraitor types (tech-stack §2.2)

This is the authoritative list of Arbitraitor public types Orchestraitor consumes. Use these
real identifiers — never conceptual names like `EffectiveSandboxControls`, `ActionPlan`, or
`ApprovalToken` (those do not compile).

| Orchestraitor concern | Arbitraitor crate | Public API used |
|---|---|---|
| Sandbox effective controls (probe) | `arbitraitor-sandbox` | `EffectiveControls`, `ControlState` enum (Available/Degraded/Unavailable), `compute_effective_controls(mode, platform)`, `SandboxMode` enum (None/Observe/Restricted/Disposable), `SandboxCapabilities` |
| Sandbox effective controls (receipt) | `arbitraitor-exec` | `EffectiveControls`, `EffectiveControl`, `ControlStatus` enum (Enforced/Partial/Unavailable), `ControlProofs`, `ExecutionContextBuilder::from_operation().into_effective_controls()` |
| Policy evaluation | `arbitraitor-policy` | `PolicyEngine::load(toml)`, `merge_layers(layers, audit_override) -> LayeredPolicy`, `evaluate(findings, ctx) -> Verdict`, `evaluate_with_trace(...) -> (Verdict, PolicyTrace)`, `EvalContext`, `PolicyPrecedence`, `PolicyLayer` |
| Approval tokens | `arbitraitor-mcp` | `ApprovalTokenIssuer::new()`, `with_secret()`, `with_durable_store()`, `PlanContext::for_bash(network_isolated, policy_snapshot_digest)`, `PlanContext::for_native(...)` |
| MCP server | `arbitraitor-mcp` | `run_stdio_server()` (inspect-only default), `build_default_server()`, `McpServer` for explicit registration of `request_approval` + `run_approved_artifact` |
| Receipts | `arbitraitor-receipt` | `Receipt`, `ReceiptBuilder`, `ApprovalInfo` (plan_digest, artifact_digest, expiry, nonce, bound_capabilities), `canonical_bytes()`, `sign_receipt()`, `verify_receipt()`, `to_intoto_statement()`, `redact_url()` |
| Wrapper-plugin plan classification | `arbitraitor-plugin-api` | `OperationPlan`, `PlannedOperation` enum, `PluginTrustClass` enum (BuiltIn/FirstParty/CommunityReviewed/CommunityUnreviewed), `CapabilitySet`, `Plugin` trait hierarchy |

> **`arbitraitor-engine` crate not yet extracted.** ADR-0038 (in Arbitraitor) describes
> extracting a consolidated `arbitraitor-engine` crate. Until then, Orchestraitor depends on
> the individual crates directly. `ApprovalTokenIssuer` currently lives in `arbitraitor-mcp`,
> not `arbitraitor-engine`.

## Capability probes (spec §16.7)

Orchestraitor must declare its minimum supported Arbitraitor version and required capability
identifiers. At startup and before protected actions, it verifies:

- Arbitraitor API compatibility;
- required capability availability;
- effective controls on the current platform;
- policy and detector digests;
- receipt schema compatibility;
- whether any requested feature is operating in degraded mode.

**A version match alone is not evidence that a control is effective. Runtime capability
reports are authoritative.**

The `orcd` daemon runs `probe_capabilities` at startup, producing a `CapabilityReport` that
classifies each required control as `Enforced`, `Partial`, or `Unavailable` (mapped from
Arbitraitor's `ControlState` / `ControlStatus`). The `health` JSON-RPC method returns this
report so clients can display and enforce the result (spec §6.7, §16.7).

## Mandatory MCP wiring (tech-stack §2.3)

The default Arbitraitor stdio MCP server is **inspect-only**. Orchestraitor must construct an
`McpServer` instance with explicitly injected `ApprovalTokenIssuer`, `ArtifactLookup`,
`ReceiptLookup`, and `PlanContext` to enable the Approve (`request_approval`) and Execute
(`run_approved_artifact`) capabilities. Treating the default server as providing those
capabilities is a **security-critical bug**.

This wiring is a Phase 0 implementation task. The MCP gateway (spec §17.2) routes and
namespaces MCP protocol operations but is not a security boundary: a proxy may block MCP calls
by refusing to route a tool invocation, but it cannot mediate filesystem syscalls made
directly by a sandboxed MCP server process. Filesystem containment is Arbitraitor's exclusive
domain (§9.4.2, §16.4).

## Fail-closed behavior (spec §6.7)

No Orchestraitor component may infer that a security control is effective merely because
configuration was requested or setup code was called. Arbitraitor must report the effective
controls for each action and platform, and Orchestraitor must display and enforce that result.

When an Arbitraitor capability is absent, unsupported, stale, or incompatible, Orchestraitor
must:

1. **block the protected operation by default;**
2. identify the missing Arbitraitor capability;
3. offer only an explicit, visibly weakened mode where policy permits;
4. record the degradation in the session and receipt;
5. direct implementation work to `arbsec/arbitraitor`, not to a parallel Orchestraitor
   control.

The `CapabilityReport` reports `fail_closed` when any required control is unavailable;
`fail_closed` takes precedence over any `degraded` status. Orchestraitor must not advertise
uniform cross-platform isolation until Arbitraitor reports equivalent effective controls for
the current platform (spec §16.8).

## Platform capability parity (spec §9.32, §16.8)

Arbitraitor currently documents strong Linux primitives but incomplete macOS and Windows
containment. Platform target order:

- **Linux** (1, MVP): reference security platform; strongest initial enforcement.
- **macOS** (2, MVP): equivalent UX + explicit capability reporting; uses
  `materialized-workspace` backend; Arbitraitor probes `seatbelt`/`sandbox-exec` and fails
  closed for strict mode if unavailable.
- **WSL2** (3, Phase 1+): Linux guest enforcement applies; Windows host actions require a
  future Windows-native broker.
- **Windows native** (4, future): separate backend, not a thin WSL wrapper. Until implemented,
  Windows users are routed to WSL2 with an explicit "Windows-native backend: not yet
  implemented" capability report.

Missing backends or controls must be implemented in Arbitraitor first (§16.2). The
architecture must not couple to any single OS mechanism (OverlayFS, FUSE, FSKit, ProjFS,
polkit, launchd); platform-neutral conceptual capabilities are the stable interface, with
actual names derived from Arbitraitor's implementation per platform.
