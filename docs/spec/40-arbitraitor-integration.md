# Orchestraitor specification: Arbitraitor integration and security

### 2.2 Security ownership invariant

Arbitraitor (`https://github.com/arbsec/arbitraitor`) is the sole implementation location and authority for every security-related capability used by Orchestraitor.

This is a hard architectural boundary:

- Orchestraitor **MUST NOT** implement an independent sandbox, policy engine, security scanner, provenance verifier, approval validator, network enforcement layer, secret enforcement layer, promotion authorization mechanism, security receipt format, or security-sensitive command classifier.
- Orchestraitor **MUST** call Arbitraitor crates, stable APIs, or an Arbitraitor service boundary for security evaluation and enforcement.
- If a required security feature is missing, it **MUST** be added to the Arbitraitor GitHub project first, including its threat model, tests, documentation, capability reporting, and fail-closed behavior.
- Orchestraitor **MUST NOT** ship a temporary duplicate, permissive fallback, compatibility no-op, private fork, or hidden alternate implementation of a missing Arbitraitor control.
- Orchestraitor **MUST** fail closed or explicitly enter a documented non-secure mode when the required Arbitraitor version or effective capability is unavailable.
- Security defects and security feature requests belong in `arbsec/arbitraitor`; Orchestraitor issues may track integration work but must link to the canonical Arbitraitor issue or pull request.
- Arbitraitor must remain independently useful and **MUST NOT** depend on Orchestraitor.

The ownership boundary is:

```text
Orchestraitor owns:
  agent loops and session orchestration
  provider and harness adapters
  CLI, TUI, GUI, IDE, MCP, ACP, and proxy integration
  context selection and token optimization
  project configuration discovery and migration
  format-on-write workflow and developer-facing diagnostics
  presentation of plans, findings, approvals, and receipts

Arbitraitor owns:
  policy evaluation and security decisions
  sandboxing and effective-control verification
  process, filesystem, network, and secret capability enforcement
  command, script, package, plugin, and artifact inspection
  provenance, signatures, trust roots, and content identity
  plan binding and approval validation
  security-sensitive output classification and promotion authorization
  tamper-evident security receipts and evidence
  fail-closed behavior and platform security capability matrices
```

Orchestraitor may translate a user or agent action into an Arbitraitor request and present the result, but it may not become an alternative security authority. Even when integration code runs inside `orcd`, the enforcement algorithm and security decision must originate from Arbitraitor-owned code.

## 6. System principles

### 6.1 The agent is always untrusted

This includes:

- direct provider models;
- proprietary agent SDKs;
- wrapped CLI harnesses;
- custom plugins;
- repository instructions;
- MCP servers;
- skills;
- generated scripts;
- build systems;
- test runners;
- compiler plugins.

A better model does not change the trust boundary.

### 6.2 A worktree is not a sandbox

Git worktrees isolate working files and branches but share repository history and Git metadata. A worker with unrestricted access to the common Git directory may modify hooks, configuration, refs, worktree metadata, or other shared state.

The trusted controller must own Git metadata and expose typed Git operations.

### 6.3 Sandboxing the process is insufficient

The agent may write a future input to a trusted host component:

- `.vscode/settings.json`
- `.vscode/tasks.json`
- `.idea` project files
- `.claude/settings.json`
- `.github/workflows`
- Git hooks or config
- virtual environment activation files
- shell startup files
- build-system plugins
- package-manager lifecycle scripts
- IDE plugins
- debugger launch configurations
- environment files
- executable binaries

The system must control which outputs are promoted to trusted host state and which host components are allowed to consume agent-written files.

### 6.4 Approval belongs to the trusted UI

Agent-generated text must never be used as the authoritative approval explanation.

The control plane constructs the approval prompt from the canonical action plan, policy evaluation, and observed sandbox capabilities.

### 6.5 Static analysis narrows authority; it does not prove safety

Arbitrary shell, Python, Node.js, build systems, compilers, and package scripts are general-purpose programs. Static analysis can:

- identify obviously dangerous behavior;
- derive required capabilities;
- provide explanations;
- choose stronger containment;
- reject unsupported opacity.

It cannot prove an arbitrary program harmless.

### 6.6 Security and performance must be measured

Claims such as "sandboxed," "low memory," "token efficient," and "safe by default" require reproducible benchmarks and receipts.

### 6.7 Arbitraitor is the sole security authority

No Orchestraitor component may infer that a security control is effective merely because configuration was requested or setup code was called. Arbitraitor must report the effective controls for each action and platform, and Orchestraitor must display and enforce that result.

When an Arbitraitor capability is absent, unsupported, stale, or incompatible, Orchestraitor must:

1. block the protected operation by default;
2. identify the missing Arbitraitor capability;
3. offer only an explicit, visibly weakened mode where policy permits;
4. record the degradation in the session and receipt;
5. direct implementation work to `arbsec/arbitraitor`, not to a parallel Orchestraitor control.

---

## 7. Threat model

### 7.1 Protected assets

- Main source checkout
- Shared Git object database and metadata
- Unrelated local files
- SSH keys
- Git credentials
- Cloud credentials
- Package-registry credentials
- Browser sessions
- API keys
- Signing keys
- Password stores
- Local service sockets
- Docker or container-engine sockets
- IDE extension host
- User shell initialization
- CI/CD credentials
- Production infrastructure
- Other agent sessions
- Trusted policy and approval state
- Audit logs and receipts

### 7.2 Adversaries

1. Malicious repository author
2. Compromised dependency or package
3. Prompt injection embedded in code, documentation, issues, logs, or web results
4. Malicious or compromised MCP server
5. Malicious plugin
6. Compromised wrapped harness
7. Hallucinating or overeager model
8. Local unprivileged attacker
9. Remote service returning crafted data
10. User accidentally granting excessive authority

### 7.3 Primary attack classes

- Direct host filesystem access
- Secret exfiltration
- Network exfiltration
- Localhost service attacks
- Container socket access
- Sandbox escape
- Confused deputy through trusted controller
- Persistent configuration injection
- Git metadata poisoning
- Build/test tool poisoning
- Dependency lifecycle execution
- IDE configuration time bombs
- Approval spoofing
- Capability escalation between turns
- Cross-session contamination
- Context poisoning
- Tool-result injection
- Denial of service through CPU, memory, disk, process, or output exhaustion
- Audit tampering
- Terminal escape-sequence attacks
- Symlink and path traversal attacks
- TOCTOU between inspection and execution

### 7.4 Explicit non-guarantees

The system cannot guarantee:

- an operating-system kernel or hypervisor has no vulnerability;
- provider-hosted inference is confidential beyond provider commitments;
- arbitrary code is semantically safe;
- a user-approved unrestricted action is safe;
- a promoted malicious code change cannot later cause harm;
- wrapped proprietary CLIs will preserve stable output formats;
- equal containment strength on Linux, macOS, and Windows;
- filesystem or shell containment when an external harness uses only the model proxy and continues executing tools outside Orchestraitor and Arbitraitor.

These limitations must be visible rather than hidden behind one `sandboxed: true` flag.

---

## 8. Trust boundaries and process model

### 8.1 Trusted components

- Core daemon
- Policy engine
- Approval renderer
- Workspace and Git controller
- Secret broker
- Network broker
- Context compiler
- Receipt signer
- TUI and GUI when authenticated to the daemon
- Thin IDE plugins for trusted UI and IDE-state collection

### 8.2 Untrusted components

- Agent workers
- Wrapped CLIs
- Direct agent loops
- Provider responses
- Repository files
- Build/test processes
- Language servers launched from the repository environment
- Third-party MCP servers
- Third-party plugins
- Generated artifacts
- Terminal output

### 8.3 Recommended process topology

```text
host
|
+-- trusted daemon
|   +-- policy engine
|   +-- workspace/Git broker
|   +-- context compiler
|   +-- secret broker
|   +-- network policy controller
|   +-- receipt/event store
|   +-- adapter supervisor
|
+-- trusted client
|   +-- TUI
|   +-- optional GUI
|   +-- IDE plugin
|
+-- isolated session environment
    +-- agent adapter
    +-- wrapped CLI or native agent runtime
    +-- build/test tools
    +-- disposable working tree
    +-- local context-query client
    +-- no ambient host credentials
```

### 8.4 IPC

Preferred local transports:

- Unix domain sockets on Linux and macOS
- Named pipes on Windows
- Mutual authentication with per-installation keys
- Optional loopback HTTP only when required by an IDE or remote client
- Short-lived session tokens
- Explicit protocol version negotiation
- Framed, bounded messages
- No unbounded terminal stream buffering

Remote workers should use mutually authenticated TLS and a separate threat model.

---

### 9.6 Arbitraitor sandbox integration

Sandbox backends and their common capability interface are owned and implemented by Arbitraitor. Orchestraitor selects a requested profile, sends the action plan to Arbitraitor, and consumes the effective-control report.

Possible Arbitraitor backends:

- Native Linux: Landlock, seccomp, namespaces, cgroups, no-new-privileges, resource limits
- Rootless Podman
- Docker with hardened profiles
- Apple Containers
- macOS sandbox profile where sufficient
- Windows AppContainer or Windows Sandbox/VM backend
- Firecracker or another microVM backend
- Daytona, Coder, E2B, or other remote sandboxes through plugins

The backend reports independent controls. The illustrative structure below previously appeared under the name `EffectiveSandboxControls`. That type **does not exist** in current Arbitraitor code; the actual identifier is `arbitraitor_sandbox::EffectiveControls` (platform capability matrix with a `ControlState` enum) for probing, and `arbitraitor_exec::EffectiveControls` (per-control proof matrix consumed by the receipt) for execution receipts. The intended shape is preserved for narrative continuity only:

```rust
// CONCEPTUAL — actual identifiers:
//   arbitraitor_sandbox::EffectiveControls  (probe matrix, ControlState enum)
//   arbitraitor_exec::EffectiveControls     (receipt matrix, Option<EffectiveControl> w/ proof)
// Fields below are the union of both; production code MUST use the real type per call site.
pub struct EffectiveSandboxControls_CONCEPTUAL_DO_NOT_USE {
    pub filesystem_isolation: ControlState,
    pub network_isolation: ControlState,
    pub process_tree_containment: ControlState,
    pub privilege_suppression: ControlState,
    pub syscall_filtering: ControlState,
    pub platform_settings_isolation: ControlState,
    pub resource_limits: ControlState,
    pub ephemeral_root: ControlState,
    pub secret_non_exposure: ControlState,
    pub host_git_isolation: ControlState,
    pub output_promotion_enforced: ControlState,
}
```

A requested policy maps to minimum required controls. Missing mandatory controls fail closed.

No code path may infer successful isolation merely because a setup function returned without error. The authoritative probes are `arbitraitor_sandbox::compute_effective_controls(mode, platform)` for the platform matrix and `arbitraitor_exec::ExecutionContextBuilder::from_operation(...)` for the receipt matrix; Orchestraitor MUST consult both, not infer from a configured-only mode flag.

### 9.8 Arbitraitor policy integration

Use Arbitraitor's layered policy model. Any generalization required for coding-agent workloads must be implemented in Arbitraitor first.

Suggested precedence:

1. Organization policy
2. Repository policy
3. User policy
4. Session tightening
5. One-time audited override

Lower layers may tighten inherited policy by default. Weakening requires a separately authenticated, audited override.

Decision outcomes:

- `pass`
- `pass_with_constraints`
- `prompt`
- `block`
- `unsupported`
- `defer_to_stronger_sandbox`

Every decision includes a trace.

### 9.9 Arbitraitor approval integration

Approval plans, binding, validation, and authorization are owned by Arbitraitor. Orchestraitor renders trusted client views from Arbitraitor-provided structured data.

> **Vocabulary note.** The approval type was previously shown as `ApprovalToken`. That struct **does not exist** in current Arbitraitor code. The actual approval surface is:
> - `arbitraitor_mcp::ApprovalTokenIssuer` — public issuer (`new()`, `with_secret(...)`, `with_durable_store(...)`, `issue()`, `validate()`). The issued token is an opaque `String` of the form `v2.<payload_hex>.<signature_hex>` (HMAC-SHA256, schema_version 3, default 5-minute lifetime).
> - `arbitraitor_mcp::ApprovalTokenPayload` — private; Orchestraitor does not construct or read it directly.
> - `arbitraitor_mcp::PlanContext` — public ADR-0013 binding context (`for_bash(network_isolated, policy_snapshot_digest)`, `for_native(...)`, …). Orchestraitor assembles this.
> - `arbitraitor_receipt::ApprovalInfo` — public receipt-recorded payload (plan_digest, artifact_digest, expiry, nonce, bound_capabilities, override_reason, override_scope, exit_status).

**Additional Arbitraitor MCP wiring required.** The default Arbitraitor MCP stdio server (`arbitraitor_mcp::run_stdio_server()` via `build_default_server()`) registers ONLY inspect-class tools: `inspect_url`, `fetch_artifact`, `scan_artifact`, `query_receipt`, `explain_verdict`. The `request_approval` (Approve-class) and `run_approved_artifact` (Execute-class) tools are NOT registered by default; they require explicit construction with injected `ApprovalTokenIssuer`, `ArtifactLookup`, `ReceiptLookup`, and `PlanContext`. Orchestraitor MUST construct an `McpServer` instance with these dependencies wired; treating the default stdio server as providing approval or execution capabilities is a security-critical bug.

Approval types:

- One action
- Repeated identical action
- Capability for current turn
- Capability for session
- Capability for repository policy
- Time-limited capability
- Destination-specific network capability
- Read-only or write-scoped Git capability

The UI must show:

- operation;
- executable identity;
- arguments;
- paths;
- network destinations;
- secret use without secret value;
- sandbox controls;
- expected outputs;
- static findings;
- policy rule;
- scope and expiry;
- whether the action affects host-trusted state.

The agent cannot approve its own request. Agent prose is shown separately and marked untrusted.

### 9.10 Arbitraitor command and script analysis

Arbitraitor must use parsers rather than regular-expression allowlists where feasible. Orchestraitor may request analysis and present findings but may not maintain a separate security classifier.

Initial analyzers:

- POSIX shell AST
- Bash-specific constructs
- PowerShell AST
- Windows command line
- Package-manager commands
- Git command semantics
- Python and Node launch classification
- Build-tool classification
- Redirection and pipeline analysis
- Environment mutation
- Subshell and command substitution
- Interpreter chaining
- Archive extraction destinations

Classification should derive capabilities, not only assign a danger score.

Examples:

- `git diff` requires repository read.
- `git push` requires network plus a repository-scoped credential.
- `npm install` requires package-registry access and lifecycle-script policy.
- `cargo test` requires process execution, workspace write, target-cache write, and possibly network if dependencies are absent.
- `bash -c "$UNTRUSTED"` is opaque and should require stronger containment or block under strict policy.
- `docker build` is not safe merely because `docker` is on an allowlist.

### 9.11 Arbitraitor package-manager gate

Use and extend Arbitraitor package-manager adapters.

Required managers:

- npm
- pnpm
- Yarn Classic
- Yarn Berry
- Bun
- Cargo
- uv/uvx
- pip
- Poetry
- Go modules
- Maven
- Gradle
- NuGet

Capabilities:

- lockfile inspection;
- provenance and checksum verification;
- lifecycle-script detection;
- registry allowlisting;
- package-name confusion checks;
- package archive inspection;
- script execution in a nested or disposable environment;
- cached dependency stores separated from host user state;
- receipts for installed artifacts.

### 9.12 Arbitraitor network enforcement

Arbitraitor owns network policy and enforcement. Default policy is no network from worker processes except the provider transport required by the agent.

Preferred design:

- worker has no direct egress;
- requests pass through a broker or sidecar proxy;
- policy can match hostname, port, scheme, method, path, query constraints, and purpose;
- DNS rebinding and private-address resolution are blocked;
- loopback and host-gateway access are denied unless explicitly granted;
- package registries can be mediated separately;
- response size and content type are bounded;
- downloads may pass through Arbitraitor inspection before release.

Provider traffic may be handled by the trusted daemon instead of the worker in direct-provider mode.

### 9.13 Arbitraitor secret enforcement

Arbitraitor owns secret capability policy, credential release, and enforcement. The preferred design is capability use without secret disclosure.

Examples:

- GitHub push through a broker that injects a scoped token
- Package-registry download through a proxy
- Cloud API call through a signed request broker
- Provider request executed by the trusted daemon
- Temporary SSH certificates rather than long-lived private keys

Secret grants specify:

```rust
pub struct SecretGrant {
    pub secret_id: SecretId,
    pub operation: SecretOperation,
    pub destination: DestinationConstraint,
    pub repository: Option<RepositoryConstraint>,
    pub expiry: Timestamp,
    pub max_uses: u32,
}
```

Raw secrets should only be mounted into a worker when a provider or tool cannot work through a broker and the user explicitly accepts the weaker boundary.

### 9.14 Arbitraitor-backed output quarantine and promotion

This is a defining subsystem. Security-sensitive classification, policy, and promotion authorization are owned by Arbitraitor; Orchestraitor owns the developer workflow and presentation.

All worker output begins untrusted.

Output classes include:

- ordinary source
- tests
- generated source
- executable
- package archive
- dependency lockfile
- IDE configuration
- shell configuration
- Git configuration
- Git hook
- agent configuration
- CI workflow
- build-system plugin
- environment file
- credential-shaped data
- symlink
- device or special file

Promotion pipeline:

```text
worker change
  -> classify changed path and content
  -> scan artifact
  -> detect trust-sensitive destination
  -> generate semantic and textual diff
  -> run policy
  -> prompt when required
  -> copy/apply through trusted controller
  -> emit promotion receipt
```

The trusted IDE plugin must not automatically open a session directory as a fully trusted project when it contains unpromoted project configuration. It should use restricted or untrusted workspace mode where supported.

### 9.23 Authentication, secret resolution, and provider wiring

This section defines how provider API keys and other secrets are resolved, stored in memory, and scoped. Security enforcement (capability release, broker-mediated secret injection into workers, raw-secret mount authorization) remains owned by Arbitraitor per §2.2 and §9.13. Orchestraitor owns the developer-facing config surface, the in-memory secret wrapper, and the auth resolver.

#### 9.23.1 Secret URI grammar

Secrets are referenced in configuration via URI-shaped strings. Plaintext literals are REFUSED in release builds (see §9.23.4).

| URI form | Resolution | Preferred use |
|---|---|---|
| `secret://keyring/<id>` | OS keyring (macOS Keychain / Windows Credential Manager / Linux Secret Service) by `<id>` under `[secrets].keyring_service` (default `"orchestraitor"`). Backed by `keyring 4.1.5` behind the optional `secrets-keyring` Cargo feature. | Developer machines. |
| `secret://env/<VAR>` | Environment variable named `<VAR>`. Aliased as `env:<VAR>`. | CI, dev containers, headless servers. |
| (plaintext literal) | The value as written. | REFUSED in release builds. Permitted only when `debug = true` AND `[secrets].disallow_plaintext_in_debug = false` (default `false` — DX convenience; set `true` to lock even dev builds). |

Env var names follow the models.dev `env` convention: `<PROVIDER>_API_KEY` (uppercase, no `ORCHESTRATOR_` prefix). Verified values: `NEURALWATT_API_KEY` (Neuralwatt), `ZHIPU_API_KEY` (Z.ai / zhipuai — NOT `ZAI_API_KEY`). See `docs/spec/tech-stack.md:§3.2` and `docs/spec/tech-stack.md:§4.3` for the in-house models.dev client that reads the `env` array from the bundled catalog.

#### 9.23.2 Resolution order

Secret URIs resolve in this order — the first form that resolves to a non-empty value wins:

```text
secret://keyring/<id>           [preferred for interactive dev — OS keyring]
  -> secret://env/<VAR>           [fallback for CI / headless]
    -> plaintext literal          [REFUSED in release; locked when disallow_plaintext_in_debug = true]
```

The auth resolver returns `secrecy::SecretString` 0.10.3 backed by `zeroize 1.9.0`. The returned `SecretString` wipe-on-drops the inner buffer, has NO `Debug` impl, and never enters a serde stream (custom `Serialize` returns `"REDACTED"`).

#### 9.23.3 Routing decision precedes auth resolution

The control plane fixes `(provider, model)` via the routing precedence chain (§9.19.2 inside each §9.22.2 config layer) BEFORE the auth resolver runs. A worker never receives a "find me a model" request that later has to infer a provider from a hostname, path, or model-id prefix — the routing decision is fixed and recorded in the per-call event. The worker receives a fully-bound `(provider_id, model_id, request, SecretString)` payload.

#### 9.23.4 In-memory secret handling and trace redaction

- The auth resolver returns `secrecy::SecretString` to the transport. The `ExposeSecret` trait is used only inside the per-provider transport adapter to inject the key into HTTP `Authorization` / `x-api-key` / `x-goog-api-key` headers.
- A redacting `tracing_subscriber::Layer` MUST omit fields whose name matches `*_key`, `*_secret`, `api_key`, `authorization`, `*_token`, `bearer`, `x-api-key`, `x-goog-api-key`, plus any value that matches a secret byte-shape heuristic (long base64 / hex / `sk-` prefix). This mirrors Arbitraitor `docs/conventions.md:92-98` (errors never leak secrets).
- No on-disk JSON secret file. The opencode `auth.json` pattern is rejected: it leaks through `tracing`, fails to atomic-rename cleanly, and requires serialization care. Orchestraitor relies on OS keyring + env.

#### 9.23.5 Provider protocol and endpoint scoping

The `[[providers]]` config block carries the explicit protocol (`openai-compatible` | `anthropic-messages` | `gemini-native`), base URL, `request_api` (for OpenAI-compatible: `chat-completions` | `responses`), `auth` URI, optional request defaults, optional subscription metadata (§9.19.5), and the per-model array. See `docs/spec/tech-stack.md:§3.2` for the concrete Neuralwatt + Z.ai examples. The `protocol` field is REQUIRED; the harness MUST NOT infer a provider protocol from a hostname or model-prefix (spec §10.3, tech-stack §3.4).

---

## 14. Security modes

### 14.1 Strict

- Disposable workspace
- No host Git metadata
- No direct network
- Brokered secrets only
- Strong output promotion
- Typed tools preferred
- Opaque shell may be blocked
- Missing controls fail closed
- IDE opens workspace as untrusted
- No raw host mounts

### 14.2 Standard, default

- Isolated workspace
- No host credentials
- Mediated network
- Static action planning
- Output promotion for sensitive classes
- Raw shell permitted inside sandbox
- Missing critical controls fail closed
- Ordinary source changes promoted through diff review

### 14.3 Compatible

- Worktree with broader tooling
- Selected credentials may be mounted read-only
- More permissive network
- Output warnings rather than blocks for some classes
- Explicit persistent warning
- Intended for harnesses that cannot work through stronger brokers

### 14.4 Host

- Current checkout
- Host execution
- Harness-native permission model
- Control plane provides observability only
- Explicit one-session override
- Red status indicator
- Receipt states that containment was unavailable

Mode names should avoid implying absolute safety. Security profiles (§21.12) map to these modes: `strict` → §14.1, `standard` → §14.2, `compatible` → §14.3, `custom` → combinations not covered by built-in profiles. Use `strict` as the recommended initial default; `orc init` MAY recommend `standard` when detected tooling requires additional capabilities. Avoid the term `relaxed`; use `compatible` because it describes the trade-off rather than implying that security no longer matters.

---

## 16. Arbitraitor integration and ownership plan

### 16.1 Non-negotiable ownership rule

Arbitraitor is not merely a collection of reusable crates or an optional backend. It is Orchestraitor's complete security subsystem.

All security functionality required by Orchestraitor must live in `arbsec/arbitraitor`, including functionality that exists primarily to support coding-agent workloads. This keeps one security model, one policy language, one capability vocabulary, one fail-closed implementation, one audit trail, and one place to review security-critical changes.

Orchestraitor may contain adapters that translate orchestration requests into Arbitraitor calls, but those adapters must not make independent allow, deny, containment, trust, approval, or promotion decisions.

### 16.2 Mandatory security feature workflow

When Orchestraitor needs a missing security capability:

1. Open or identify the canonical issue in `arbsec/arbitraitor`.
2. Define the threat model, protected assets, required effective controls, failure behavior, and receipt evidence in Arbitraitor.
3. Implement the feature in Arbitraitor-owned crates or services.
4. Add unit, property, integration, adversarial, and platform capability tests in Arbitraitor.
5. Expose a versioned Arbitraitor API and capability identifier.
6. Release or pin the required Arbitraitor revision.
7. Add only the Orchestraitor integration, UI, and workflow after the Arbitraitor capability exists.

A security-sensitive prototype may not bypass this process by placing a temporary implementation in Orchestraitor. Until Arbitraitor supports the capability, the associated Orchestraitor feature remains blocked, experimental in an explicitly non-secure mode, or out of scope.

### 16.3 Dependency direction

The dependency direction is strict:

```text
Orchestraitor
  -> Arbitraitor public crates, APIs, capability reports, and receipts

Arbitraitor
  -X-> Orchestraitor session, UI, provider, adapter, or context abstractions
```

Arbitraitor must remain independently usable for artifact inspection and controlled execution. It must not import Orchestraitor types or become coupled to a particular coding-agent UI.

### 16.4 Arbitraitor-owned capability areas

Existing or planned Arbitraitor capabilities used by Orchestraitor include:

- artifact identity, hashing, immutable content handling, and CAS;
- layered policy evaluation and decision traces;
- command, shell, PowerShell, package, plugin, and artifact analysis;
- provenance, signatures, trust roots, TUF, TOFU, minisign, and cosign;
- plan-bound approvals and capability separation;
- process, filesystem, network, secret, and resource enforcement;
- sandbox backend selection and effective-control reporting;
- workspace projection (§9.4.2): synthetic filesystem, path confinement, per-principal scopes, symlink confinement, transactional overlays, mutation attribution, and atomic promotion enforcement;
- package-manager gates and lifecycle-script inspection;
- output security classification and promotion authorization;
- tamper-evident receipts and evidence composition;
- SSRF protection and mediated downloads;
- fail-closed platform capability matrices;
- sandboxed subprocess and Wasmtime plugin security.
- platform backends for all security capabilities (§9.32): Linux namespaces + OverlayFS, macOS FSKit/service-management, WSL2 Linux-guest enforcement, Windows-native ProjFS/AppContainer/service broker; each backend owned and implemented in Arbitraitor.

The exact crate boundaries may evolve in Arbitraitor. Orchestraitor should depend on stable behavior and versioned interfaces rather than copy internal implementation details.

### 16.5 Orchestraitor-owned integration areas

Orchestraitor owns:

- native agent loop and session lifecycle;
- provider, SDK, ACP, CLI, MCP, IDE, and proxy adapters;
- project discovery, initialization, configuration import, and migration;
- context compiler, LSP integration, semantic indexing, caching, and token budgeting;
- format-on-write and lint-fix transaction orchestration;
- workspace lifecycle and Git workflow orchestration, while delegating security-sensitive permissions and enforcement to Arbitraitor;
- workspace projection configuration, backend selection, conformance testing, and enforcement-level reporting (§9.4.2); while Arbitraitor owns the projection implementation;
- platform discovery, capability reporting, cross-platform conformance testing, and per-platform enforcement-level presentation (§9.32); while Arbitraitor owns all platform backend implementations;
- TUI, GUI, CLI, IDE plugins, and developer-facing diagnostics;
- presentation of Arbitraitor plans, decisions, findings, approvals, controls, and receipts;
- compatibility testing and enforcement-level reporting.

### 16.6 Proposed repository and workspace layout

```text
arbsec/arbitraitor
├── security policy and decisions
├── sandbox and effective-control verification
├── command, package, plugin, and artifact inspection
├── network and secret enforcement
├── approval binding and validation
├── output promotion authorization
└── security receipts and evidence

arbsec/orchestraitor
├── crates/orchestraitor-core
├── crates/orchestraitor-daemon
├── crates/orchestraitor-model
├── crates/orchestraitor-arb-client
├── crates/orchestraitor-workspace
├── crates/orchestraitor-context
├── crates/orchestraitor-events
├── crates/orchestraitor-adapter-api
├── crates/orchestraitor-adapter-host
├── crates/orchestraitor-provider-api
├── crates/orchestraitor-provider-proxy
├── crates/orchestraitor-mcp
├── crates/orchestraitor-tui
├── crates/orchestraitor-cli
├── crates/integrations/jetbrains
├── crates/integrations/vscode
├── crates/adapters/claude
├── crates/adapters/codex
├── crates/adapters/gemini
├── crates/adapters/opencode
└── crates/adapters/pi
```

Do not create Orchestraitor crates named `sandbox`, `policy`, `network-broker`, `secret-broker`, `approval`, or `security-receipt` if they would own security logic. A narrowly scoped client or presentation crate is acceptable only when its name and API make the delegation to Arbitraitor unambiguous.

### 16.7 Versioning and capability negotiation

Orchestraitor must declare its minimum supported Arbitraitor version and required capability identifiers. At startup and before protected actions, it must verify:

- Arbitraitor API compatibility;
- required capability availability;
- effective controls on the current platform;
- policy and detector digests;
- receipt schema compatibility;
- whether any requested feature is operating in degraded mode.

A version match alone is not evidence that a control is effective. Runtime capability reports are authoritative.

### 16.8 Platform limitation and capability parity

Arbitraitor currently documents strong Linux primitives but incomplete macOS and Windows containment. Orchestraitor MUST NOT advertise uniform cross-platform isolation until Arbitraitor reports equivalent effective controls (§9.32.4 capability report).

Platform target order per §9.32:

- **Linux** (1, MVP): reference security platform. Strongest initial enforcement. see §9.32.3.1.
- **macOS** (2, MVP): equivalent UX + explicit capability reporting. Uses `materialized-workspace` backend (same `gix` snapshot as Linux). Arbitraitor probes `seatbelt`/`sandbox-exec`; fails closed for strict mode if unavailable, offers `standard` degraded mode where policy permits. see §9.32.3.2.
- **WSL2** (3, Phase 1+): Linux guest enforcement applies; Windows host actions require future Windows-native broker. see §9.32.3.3.
- **Windows native** (4, future): separate backend, not a thin WSL wrapper. see §9.32.3.4. Until implemented, Windows users are routed to WSL2 with explicit "Windows-native backend: not yet implemented" capability report.

Missing backends or controls MUST be implemented in Arbitraitor first (§16.2). Orchestraitor records the gap and either fails closed or runs in an explicitly-degraded mode per §6.7 + §9.32.6 ("Never advertise a stronger guarantee than the active platform backend can enforce").

The architecture MUST NOT couple to any single OS mechanism (OverlayFS, FUSE, FSKit, ProjFS, polkit, launchd). Platform-neutral conceptual capabilities (§9.32.1) are the stable interface; actual names are derived from Arbitraitor's implementation per platform.

---

## Appendix C: Example policy


```toml
version = 1

[defaults]
action = "prompt"
non_interactive_prompt_action = "block"
fail_closed_on_unavailable = true

[workspace]
mode = "snapshot"
host_git_access = "deny"
host_checkout_access = "deny"
promote_sensitive_outputs = true

[sandbox.require]
filesystem_isolation = true
network_isolation = true
process_tree_containment = true
privilege_suppression = true
resource_limits = true
host_git_isolation = true
output_promotion = true

[network]
default = "deny"
block_private_networks = true
block_loopback = true
require_https = true

[[network.services]]
id = "npm-registry"
host = "registry.npmjs.org"
methods = ["GET"]
paths = ["/**"]

[[rules]]
id = "allow-read-tools"
action = "pass"
when.operation = ["file.read", "context.query", "git.diff"]

[[rules]]
id = "allow-tests-in-sandbox"
action = "pass_with_constraints"
when.operation = ["process.test"]
constraints.network = "deny"
constraints.max_cpu_seconds = 600
constraints.max_memory_mb = 4096

[[rules]]
id = "prompt-package-install"
action = "prompt"
when.operation = ["package.install"]

[[rules]]
id = "block-host-config"
action = "block"
when.output_class = [
  "shell_config",
  "git_config",
  "git_hook",
  "credential",
]

[[rules]]
id = "prompt-ide-config-promotion"
action = "prompt"
when.operation = ["output.promote"]
when.output_class = ["ide_config", "ci_workflow", "agent_config"]
```

---
