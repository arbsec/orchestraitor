# Orchestraitor technology stack and bootstrap baseline

**Status:** Recommended implementation baseline correlated with spec revision 0.14.
**Version:** 0.14
**Research date:** 2026-07-23
**Scope:** Rust implementation, dependencies, repository architecture, CI/CD, OSS governance, Arbitraitor integration surface, and MVP milestone scope.

> **Versioning note.** Crate versions and the Arbitraitor git revision listed below were verified against crates.io metadata, GitHub, and the Arbitraitor repository on 2026-07-23. They go stale quickly. Before starting implementation, re-verify every version against the latest crates.io / GitHub and update if they don't match. The Arbitraitor git revision should be bumped to the latest `main` HEAD before starting work and updated weekly thereafter; the CI "Arbitraitor bump" job (§17) catches API drift.

> **Companion to:** [`docs/spec/spec.md`](spec.md). This document holds concrete crates, versions, license compatibility, runtime dependencies, platform support, and rejected alternatives. The spec holds product requirements, architecture, invariants, workflows, security boundaries, and user experience. Both files use the real Arbitraitor crate names, types, traits, and APIs (see §16 of the spec).
>
> **Structural inspiration:** [arbsec/arbitraitor `docs/spec/tech-stack.md`](https://github.com/arbsec/arbitraitor/blob/main/docs/spec/tech-stack.md). Every dependency and architectural claim below was independently verified against crates.io metadata or upstream GitHub on 2026-07-23.

---

## 1. Recommended baseline

> **Versions verified on 2026-07-23.** Re-verify against crates.io / GitHub before implementation; update if they don't match. The Arbitraitor git revision should be bumped to the latest `main` HEAD before starting work and weekly thereafter.

| Concern | Recommendation | License | Verified |
|---|---|---|---|
| Language | Rust 2024 edition | — | — |
| Bootstrap toolchain | Rust 1.96.0, pinned in `rust-toolchain.toml` | — | matches Arbitraitor MSRV |
| Workspace resolver | Cargo resolver 3 | — | — |
| Async runtime | Tokio (current_thread default for the daemon) | MIT | crates.io, 17M dl/wk |
| TUI framework | Ratatui 0.30.2 + crossterm 0.29 | MIT | crates.io, ratatui-org |
| Inline images (optional) | ratatui-image 11.0.6, feature-gated | MIT | crates.io |
| HTTP client | reqwest 0.13.4 (rustls default) | MIT OR Apache-2.0 | crates.io |
| HTTP server / proxy | hyper 1.11 + hyper-util + tower 0.5 | MIT | crates.io |
| TLS | rustls 0.23.42 + aws-lc-rs 1.17.3 provider | Apache-2.0 OR ISC OR MIT | crates.io |
| JSON for protocols | serde_json 1.0.151 (preserve_order) | MIT OR Apache-2.0 | crates.io |
| TOML config | toml 1.1.3 + toml_edit | MIT OR Apache-2.0 | crates.io |
| Layered config | figment 0.10.19 (maintenance mode; serde+toml escape hatch planned) | MIT OR Apache-2.0 | crates.io |
| Receipt canonicalization | serde_json_canonicalizer 0.3.2 (RFC 8785 JCS) | MIT | crates.io |
| CLI | clap 4.6.4 + clap_derive | MIT OR Apache-2.0 | crates.io |
| Git (controller-owned) | gix 0.85.0 with `bail_if_untrusted()` | MIT OR Apache-2.0 | crates.io |
| Tree-sitter baseline indexer | tree-sitter 0.26.11 + per-language grammar crates (all MIT) | MIT | crates.io |
| MCP server + client | rmcp 2.2.0 (official MCP Rust SDK) | Apache-2.0 | crates.io, modelcontextprotocol/rust-sdk |
| Agent Client Protocol | agent-client-protocol 1.3.0 + agent-client-protocol-schema | Apache-2.0 | crates.io, agentclientprotocol/rust-sdk |
| ACP ↔ MCP bridge | agent-client-protocol-rmcp 1.3.0 | Apache-2.0 | crates.io |
| JSON-RPC (non-MCP) | jsonrpsee 0.26.0 | MIT | crates.io |
| PTY | portable-pty 0.9.0 | MIT | crates.io |
| Errors (lib) | thiserror 2 | MIT OR Apache-2.0 | crates.io |
| Errors (CLI boundary) | miette (latest 7.x) | MIT | crates.io |
| Logging / telemetry | tracing 0.1.44 + tracing-subscriber 0.3.23 (env-filter, json) | MIT | crates.io |
| Storage (metadata) | SQLite via rusqlite 0.x with WAL mode | MIT | crates.io |
| Storage (blobs) | Filesystem CAS by SHA-256 (same layout as Arbitraitor `store/`) | — | — |
| Sandbox (Linux) | landlock 0.4.5 + rustix 1.1.4 + nix 0.31.3 | MIT OR Apache-2.0; Apache-2.0 WITH LLVM-exception; MIT | crates.io |
| Receipt signing | ed25519-dalek 3.0.0; minisign-compat where Arbitraitor calls for it | BSD-3-Clause | crates.io |
| Provenance | sigstore 0.14.0 (cosign + Rekor; opt-in feature) | Apache-2.0 | crates.io |
| Secrets | secrecy 0.10.3 + zeroize 1.9.0 + keyring 4.1.5 | Apache-2.0 OR MIT | crates.io |
| Schemas | schemars 1.0 (matched to Arbitraitor's version), JSON Schema 2020-12 | MIT OR Apache-2.0 | crates.io |
| Provider baseline (MVP) | genai 0.6.5 (feature-gated) behind project-owned `ProviderTransport` trait; raw reqwest escape hatch when native extensions are needed | MIT | crates.io |
| LSP types (client of wrapped harness LSP) | lsp-types 0.97.0 | MIT | crates.io |
| Optional GUI (later) | Tauri 2.x, behind a `gui` Cargo feature; default build is headless | Apache-2.0 OR MIT | crates.io |
| Test runner | cargo-nextest | MIT OR Apache-2.0 | crates.io |
| Feature matrix | cargo-hack | MIT OR Apache-2.0 | crates.io |
| Property testing | proptest | MIT OR Apache-2.0 | crates.io |
| Snapshot testing | insta | MIT | crates.io |
| Fuzzing | cargo-fuzz (libFuzzer) | MIT OR Apache-2.0 | crates.io |
| Miri / unsafe boundary | cargo miri | MIT OR Apache-2.0 | rust-lang |
| Mutation testing | cargo-mutants (scheduled CI) | MIT | crates.io |
| Coverage | cargo-llvm-cov | MIT | crates.io |
| Dependency policy | cargo-deny + cargo-audit + GitHub dependency review | — | — |
| Dependency trust | cargo-vet (progressive adoption) | MIT OR Apache-2.0 | crates.io |
| API compatibility | cargo-semver-checks (once any crate publishes to crates.io) | MIT OR Apache-2.0 | crates.io |
| Deterministic provider simulator | orchestraitor-testkit (in-house, no live provider in CI) | — | Orchestraitor |
| Lint policy | workspace `forbid(unsafe_code)` in core crates; `deny(missing_docs, unwrap_used, expect_used, panic, unimplemented, dbg_macro, print_stdout, print_stderr)`; `warn(pedantic, cargo)` — matches Arbitraitor conventions | — | — |

Every crate above has been verified as compatible with the dual `MIT OR Apache-2.0` license Orchestraitor shares with Arbitraitor. No GPL, AGPL, BUSL, SSPL, or Unlicense crate is present. No MPL-2.0 crate is in the MVP stack.

---

## 2. Arbitraitor dependency model

### 2.1 Hard constraint: no published crates

As of 2026-07-23, ALL Arbitraitor workspace crates set `publish = false` (28 of 28 crates verified). Orchestraitor MUST NOT depend on Arbitraitor via crates.io. Two integration paths:

1. **Git dependency pinned to a specific commit** (recommended for tagged releases).
2. **Local path override** (for development against `../arbitraitor`).

Recommended pattern using Cargo's `[patch]` section:

```toml
# Cargo.toml (workspace root)
# NOTE: Replace <LATEST_REV> with the latest main HEAD from github.com/arbsec/arbitraitor
# before starting work. Update weekly. The CI "Arbitraitor bump" job catches API drift.
[workspace.dependencies]
arbitraitor-core       = { git = "https://github.com/arbsec/arbitraitor.git", rev = "<LATEST_REV>" }
arbitraitor-model      = { git = "https://github.com/arbsec/arbitraitor.git", rev = "<LATEST_REV>" }
arbitraitor-policy     = { git = "https://github.com/arbsec/arbitraitor.git", rev = "<LATEST_REV>" }
arbitraitor-sandbox    = { git = "https://github.com/arbsec/arbitraitor.git", rev = "<LATEST_REV>" }
arbitraitor-exec       = { git = "https://github.com/arbsec/arbitraitor.git", rev = "<LATEST_REV>" }
arbitraitor-receipt    = { git = "https://github.com/arbsec/arbitraitor.git", rev = "<LATEST_REV>" }
arbitraitor-mcp        = { git = "https://github.com/arbsec/arbitraitor.git", rev = "<LATEST_REV>" }
arbitraitor-plugin-api = { git = "https://github.com/arbsec/arbitraitor.git", rev = "<LATEST_REV>" }

# Cargo.toml (per-crate)
[dependencies]
arbitraitor-sandbox = { workspace = true }
arbitraitor-receipt = { workspace = true }
arbitraitor-mcp     = { workspace = true }
```

Local development override (NOT checked in):

```toml
# .cargo/config.toml or ~/.cargo/config.toml
# NOTE: Adjust "../arbitraitor" to wherever you cloned arbsec/arbitraitor locally.
# Clone: git clone https://github.com/arbsec/arbitraitor.git ../arbitraitor
[patch."https://github.com/arbsec/arbitraitor.git"]
arbitraitor-core       = { path = "../arbitraitor/crates/arbitraitor-core" }
arbitraitor-model      = { path = "../arbitraitor/crates/arbitraitor-model" }
arbitraitor-policy     = { path = "../arbitraitor/crates/arbitraitor-policy" }
arbitraitor-sandbox    = { path = "../arbitraitor/crates/arbitraitor-sandbox" }
arbitraitor-exec       = { path = "../arbitraitor/crates/arbitraitor-exec" }
arbitraitor-receipt    = { path = "../arbitraitor/crates/arbitraitor-receipt" }
arbitraitor-mcp        = { path = "../arbitraitor/crates/arbitraitor-mcp" }
arbitraitor-plugin-api = { path = "../arbitraitor/crates/arbitraitor-plugin-api" }
```

The Arbitraitor git revision is pinned to the latest `main` HEAD at implementation start time and updated weekly. Exact-rev pinning is the only safe option because Arbitraitor's API surface is pre-1.0 and may shift commit-to-commit. The CI "Arbitraitor bump" job (§17) runs the Orchestraitor test suite against the latest `main` weekly; red status is informative, not blocking. When Arbitraitor publishes a semver tag, switch to `tag = "v0.x.y"` for cleaner dependency management.

> **`arbitraitor-engine` crate not yet extracted.** ADR-0038 (`docs/adr/0038-pipeline-engine-crate-extraction.md` in Arbitraitor) describes extracting a consolidated `arbitraitor-engine` crate from individual crates. The extraction is staged (ADR-0038 Stage 0-8). Until the engine crate is published, Orchestraitor depends on the individual Arbitraitor crates (`arbitraitor-sandbox`, `arbitraitor-policy`, `arbitraitor-mcp`, `arbitraitor-receipt`, etc.) directly. Once ADR-0038's extraction lands and `arbitraitor-engine` publishes `InspectionResultReceipt` (the engine-owned wrapper for receipt types per ADR-0038 decision 6), Orchestraitor should consume the wrapper instead of raw `arbitraitor-receipt` types. Until then, direct dependency on `arbitraitor-receipt` is the only option.

> **`ApprovalTokenIssuer` currently lives in `arbitraitor-mcp`**, not `arbitraitor-engine`. ADR-0039 (proposed) will decide whether approval flow moves to `arbitraitor-engine`. Until then, Orchestraitor depends on `arbitraitor-mcp` for approval flow.

> **`schemars` version alignment.** Arbitraitor uses `schemars = { version = "1.0", features = ["uuid1", "url2"] }`. Orchestraitor MUST use `schemars 1.0` (not `0.8.x`) to match — otherwise schema types shared across crate boundaries would be incompatible. Bumped from the initial `0.8.x` recommendation.

### 2.2 Arbitraitor integration surface used by Orchestraitor

This is the authoritative list of Arbitraitor public types Orchestraitor consumes. Spec §9 and §16 still hold the narrative; this table is the engineering ground truth.

| Orchestraitor concern | Arbitraitor crate | Public API used | Notes |
|---|---|---|---|
| Sandbox effective controls (probe) | `arbitraitor-sandbox` | `EffectiveControls` struct, `ControlState` enum (Available/Degraded/Unavailable), `compute_effective_controls(mode, platform)`, `SandboxMode` enum (None/Observe/Restricted/Disposable), `SandboxCapabilities` struct | `crates/arbitraitor-sandbox/src/lib.rs:212,186,387,41,132` |
| Sandbox effective controls (receipt) | `arbitraitor-exec` | `EffectiveControls`, `EffectiveControl`, `ControlStatus` enum (Enforced/Partial/Unavailable), `ControlProofs`, `ExecutionContextBuilder::from_operation().into_effective_controls()` | `crates/arbitraitor-exec/src/lib.rs:805,789,848,1133,885` |
| Policy evaluation | `arbitraitor-policy` | `PolicyEngine::load(toml)`, `merge_layers(layers, audit_override) -> LayeredPolicy`, `evaluate(findings, ctx) -> Verdict`, `evaluate_with_trace(...) -> (Verdict, PolicyTrace)`, `EvalContext`, `PolicyPrecedence`, `PolicyLayer` | `crates/arbitraitor-policy/src/engine.rs:60,110,174,188,19,34` |
| Approval tokens | `arbitraitor-mcp` | `ApprovalTokenIssuer::new()`, `with_secret()`, `with_durable_store()`, public `PlanContext::for_bash(network_isolated, policy_snapshot_digest)`, `PlanContext::for_native(...)` | `crates/arbitraitor-mcp/src/lib.rs:882,712` |
| MCP server (inspect-only default) | `arbitraitor-mcp` | `run_stdio_server()` (registers inspect_url / fetch_artifact / scan_artifact / query_receipt / explain_verdict), `build_default_server()`, `McpServer` for explicit registration of `request_approval` + `run_approved_artifact` | `crates/arbitraitor-mcp/src/lib.rs:2107,1981` |
| Receipts | `arbitraitor-receipt` | `Receipt`, `ReceiptBuilder`, `ApprovalInfo` (plan_digest, artifact_digest, expiry, nonce, bound_capabilities), `canonical_bytes()`, `sign_receipt()`, `verify_receipt()`, `to_intoto_statement()`, `redact_url()` | `crates/arbitraitor-receipt/src/lib.rs:34,503,126,151,716,750,180,795` |
| Wrapper-plugin plan classification | `arbitraitor-plugin-api` | `OperationPlan` (the plugin-api version), `PlannedOperation` enum, `PluginTrustClass` enum (BuiltIn/FirstParty/CommunityReviewed/CommunityUnreviewed), `CapabilitySet`, `Plugin` trait hierarchy | `crates/arbitraitor-plugin-api/src/lib.rs:200,306,32,46,471` |

Conceptual names used in older spec revisions and their replacements:

| Conceptual name (deprecated) | Real type | Why it matters |
|---|---|---|
| `EffectiveSandboxControls` | `arbitraitor_sandbox::EffectiveControls` (probe) and `arbitraitor_exec::EffectiveControls` (receipt) — two different structs with different shapes | Code referencing `EffectiveSandboxControls` will not compile. |
| `ActionPlan` | `arbitraitor_plugin_api::OperationPlan`, `arbitraitor_model::operation::OperationPlan`, or `arbitraitor_mcp::PlanContext` depending on the call site | Code referencing `ActionPlan` will not compile. |
| `ApprovalToken` | `arbitraitor_mcp::ApprovalTokenIssuer` (issuer), opaque `String` token (`v2.<payload_hex>.<signature_hex>`), `arbitraitor_receipt::ApprovalInfo` (receipt record) | No struct of this name exists; the issued token is an opaque String. |

### 2.3 Mandatory Arbitraitor MCP wiring

The default Arbitraitor stdio MCP server is inspect-only. Orchestraitor MUST construct an `McpServer` instance with explicitly injected `ApprovalTokenIssuer`, `ArtifactLookup`, `ReceiptLookup`, and `PlanContext` to enable the Approve (`request_approval`) and Execute (`run_approved_artifact`) capabilities. Treating the default server as providing those capabilities is a security-critical bug. This wiring is a Phase 0 implementation task; see the plan in `.omo/plans/orchestraitor-mvp-bootstrap.md`.

### 2.4 Upstream Arbitraitor prerequisites

Orchestraitor features that depend on Arbitraitor capabilities NOT yet present in the pinned revision are explicit upstream prerequisites in the plan. They are NOT implemented in Orchestraitor. Defaults: fail closed per spec §6.7.

---

## 3. GLM-5.2 provider integration

### 3.1 Endpoints

| Endpoint | Operator | Use |
|---|---|---|
| `https://api.neuralwatt.com/v1` | Neuralwatt | **MVP target.** OpenAI Chat Completions shape (`/chat/completions`), model id `glm-5.2`. Neuralwatt is the primary BYOK target. |
| `https://api.z.ai/api/paas/v4/` | Z.ai | Alternate OpenAI Chat Completions shape; same underlying model. Z.ai also exposes an Anthropic Messages compat gateway at `https://api.z.ai/api/anthropic` for the GLM Coding Plan (model id `glm-5.2` or `glm-5.2[1m]` for 1M context). |
| ~~`https://open.bigmodel.cn/api/paas/v4/`~~ | (legacy Zhipu) | **Deprecated branding.** Must NOT ship as a default. Config loader accepts it for backward compatibility with user-supplied configs. |

Both OpenAI OpenAI-Chat-Completions-compatible. No first-party Rust SDK from any of Z.ai, Neuralwatt, BigModel. Orchestraitor uses `reqwest` 0.13.4 (default-features off, `json + stream + rustls-tls`) directly, with `genai` 0.6.5 as the primary transport implementation behind the project-owned `ProviderTransport` trait (spec §10.2). `async-openai` 0.41.1 behind `provider-openai-escape`, `claude-api` 0.5.3 behind `provider-anthropic-escape`, `gemini-rust` 2.0.0 (post-MVP) behind `provider-gemini-escape`. Each third-party crate is isolated behind a crate-local adapter so the project trait is the public surface.

A cassette test for `/v1/models` and `/v1/chat/completions` ships with the MVP for the Neuralwatt+GLM-5.2 path. All transactions run through `secrecy::SecretString` for the API key; the key never enters a serialized serde stream (custom `Serialize` returns `REDACTED`).

### 3.2 Endpoint environment variables + auth convention

Orchestraitor follows the models.dev `env` convention: per-provider API key env var names are `<PROVIDER>_API_KEY` (uppercase). The env var name is NOT prefixed with `ORCHESTRATOR_` — it matches what models.dev catalogues (and what the vendor docs publish), so users who already have `NEURALWATT_API_KEY` set for other tools don't need a second env var. Pin to the env var name models.dev publishes for that provider.

| Provider | Env var (models.dev verified 2026-07-23) | Base URL |
|---|---|---|
| Neuralwatt | `NEURALWATT_API_KEY` | `https://api.neuralwatt.com/v1` |
| Z.ai | `ZHIPU_API_KEY` (NOT `ZAI_API_KEY` — Z.ai reuses the legacy Zhipu env var) | `https://api.z.ai/api/paas/v4/` |

Auth resolution order (lowest to highest authority — first match wins):

1. `secret://keyring/<id>` — OS keyring (macOS Keychain / Windows Credential Manager / Linux Secret Service). Preferred for developer machines. Backed by `keyring 4.1.5` behind the optional `secrets-keyring` Cargo feature.
2. `secret://env/<VAR>` (or `env:<VAR>`) — env var lookup. Default for CI, dev containers, headless servers.
3. Plaintext — REFUSED in release builds. Behind `debug = true` only; `orchestraitor-doctor` warns. Refused entirely when `[secrets].disallow_plaintext_in_debug = true` (default false for DX).

In-memory representation is `secrecy::SecretString` 0.10.3 backed by `zeroize 1.9.0` — wipe on drop, no `Debug` impl. The auth resolver returns `SecretString` and never persists the secret, never logs it, never re-serializes it. Arbitraitor `conventions.md:92-98` rule applies: secrets, headers, cookies, signed URLs, approval tokens, and secret-broker payloads MUST NOT appear in error strings or traces; a redacting `tracing_subscriber::Layer` omits fields whose name matches `*_key`, `*_secret`, `api_key`, `authorization`, `*_token`, `bearer`, `x-api-key`, `x-goog-api-key`.

Config forms (precedence: `secret://keyring/<id>` preferred; `secret://env/<VAR>` fallback; plaintext for local dev only, never committed):
```toml
[providers.neuralwatt]
protocol = "openai-compatible"           # REQUIRED: "openai-compatible" | "anthropic-messages" | "gemini-native"
base_url = "https://api.neuralwatt.com/v1" # required for "openai-compatible"
request_api = "chat-completions"          # required for "openai-compatible"
auth = "secret://keyring/neuralwatt"      # preferred (OS keyring); keyring_service = "orchestraitor" by default
# auth = "secret://env/NEURALWATT_API_KEY" # fallback
# auth = "sk-..."                          # local dev only; never commit
discovery = "models-endpoint"             # "models-endpoint" | "static" | "off"; default "models-endpoint"
probe_discovery = true                    # call GET <base>/v1/models at first use; cache forever
healthcheck_at_startup = true

# Optional: per-provider request defaults applied to every call to this provider
# default_request = { max_tokens = 32768, temperature = 1.0, reasoning_effort = "high" }

# Optional: extra HTTP headers on every request (NEVER Authorization — that's the auth layer)
# headers = { "X-Org-Id" = "..." }

# Optional: provider-specific body overlays (MVP = off; use sparingly to avoid divergence)
# body_overrides = { "service_tier" = "flex" }
```

### 3.3 Per-model declarations

```toml
[[providers.models]]
id                   = "glm-5.2"          # required; unique within this provider
context_window       = 1_048_560         # required; Neuralwatt reports 1M + 8-token overhead
max_output_tokens    = 1_048_560         # required; Neuralwatt exposes full context as output
tool_call            = true              # optional; default false
interleaved_thinking = true              # optional; preserves reasoning across turns for GLM-5.2
metadata_from        = "manual"          # optional; "models.dev:neuralwatt/glm-5.2" once confirmed, OR "manual" to forbid fuzzy hints
# api_id             = "glm-5.2"         # optional override of the wire name sent in the request
# fallback_provider  = "zai"             # optional alternate provider for the same model id
```

The same TOML shape applies to other providers. For Z.ai the GLM Coding Plan exposes an Anthropic-compatible surface; a second `[[providers]]` block with `protocol = "anthropic-messages"` and `base_url = "https://api.z.ai/api/anthropic"` serves the same `glm-5.2` model id from the worker's perspective.

### 3.4 Provider inference rule (re-emphasis)

`provider` is fixed at routing time, not at URL or model-id time. The harness never inspects a hostname, path, or model-id prefix to decide which protocol to use. The four pieces of evidence are: (a) `route.<layer>.<role|domain> = { provider, model }` — the routing decision is recorded in the per-call event; (b) the `[[providers]]` block declares protocol + base URL + auth; (c) the auth resolver returns a `SecretString`; (d) the worker constructs the request from `(provider.protocol, provider.base_url, model.id, request, secret)` and dispatches. The routing decision fixes `(provider, model)` BEFORE the auth resolver runs. Spec §10.3 explicit-protocol-first is enforced here, not at probe time.

---

## 4. ACP and MCP library pinning

### 4.1 MCP

`rmcp` 2.2.0 (Jul 8, 2026), maintained at `modelcontextprotocol/rust-sdk`, Apache-2.0. ~917k dl/wk. Features used: `server`, `client`, `macros`, `schemars`, `auth`, `transport-io`, `transport-streamable-http-server`, `transport-streamable-http-client-reqwest`, `reqwest`. TLS defaults to rustls via the reqwest feature.

### 4.2 ACP

`agent-client-protocol` 1.3.0 (Jul 20, 2026), maintained at `agentclientprotocol/rust-sdk` (org moved away from `zed-industries`; that path redirects), Apache-2.0. ~102k dl/wk. Companion crates `agent-client-protocol-schema`, `-http`, `-rmcp` (MCP bridge), `-tokio`, `-derive`, `-conductor`. Wire-stable since v1.0 (Jun 29, 2026).

> Verify the pinned release before implementation. The plan calls for a release pin and a CI gate (cargo-deny + a custom smoke check that issues an ACP `initialize` against the bundled conductor stub).

### 4.3 models.dev metadata client (in-house, live-first with cache + bundled fallback)

No external `models-dev` Rust crate is used. The published `models-dev 0.1.1` (verbalshadow) is insufficient for spec §10.4 requirements (no schema validation, no timeout/size limits, no per-field digests, no rollback, inactive since 2025-09-23). Orchestraitor ships an **in-house** client in `orchestraitor-provider-meta` crate (~150 LoC).

**Behavior priority (live-first, cached, bundled-last):**

1. On startup (after the daemon is ready, NOT on the cold-startup path — see spec §13.3.1), fetch `https://models.dev/catalog.json` (3.4 MB; the single superset carrying both `providers` ≡ api.json and `models` ≡ models.json + benchmarks/weights).
2. Validate, store by digest, cache on disk at `<cache_dir>/orchestraitor/models-dev/<digest>.json` (default `<cache_dir>` = `~/.cache/orchestraitor` on Linux; spec follows XDG via `dirs` crate).
3. Refresh asynchronously in the background — never on the critical startup path. Refresh cadence: 5-minute TTL in memory, 60-minute background refresh (mirrors opencode's `packages/core/src/models-dev.ts` cadence).
4. If the live fetch fails after the retry budget (per spec §9.26), fall back to the latest cached snapshot; if no local cache exists, fall back to the bundled snapshot compiled into the binary via `include_bytes!("data/catalog.json")`.
5. The user is told (via the §13.3.1 startup progress indicator) which path actually served the catalog: `live`, `cached (digest=f3a2...)`, or `bundled fallback (may be stale; refresh recommended)`.

**HTTP cache validators**: respect `Cache-Control: public, max-age=3600` (set by the models.dev worker); use `If-None-Match` / `If-Modified-Since`; 304 responses count as success.

**Download limits**: enforce size (reject >10 MB), timeout (≤30 s connect, ≤60 s total), content type (must be `application/json`), and redirect count (≤5).

**Schema validation**: validate against a locally versioned permissive JSON Schema; preserve unknown fields for forward compatibility.

**Retrieval metadata**: record retrieval time + catalog digest + serving path (live/cached/bundled) in the event store; surface in `orc doctor` output.

**Rollback**: `orc models rollback` rolls back to the previous cached snapshot; never removes manually-configured models because the catalog is missing them (spec §10.4 L1812).
**Refresh**: `orc models refresh` forces an immediate fetch + cache update.

**Auth metadata**: models.dev encodes auth via the per-provider `env` array (e.g., `["NEURALWATT_API_KEY"]`, `["ZHIPU_API_KEY"]`). The Orchestraitor in-house client reads this field and surfaces it as the default env var name when a user has not supplied an explicit `auth` URI (§9.23.1).

### 4.4 Crate pins for provider transports

| Crate | Version | License | Role |
|---|---|---|---|
| `genai` | 0.6.5 (max stable; 0.7.0-beta in flight, NOT pinned for MVP) | MIT OR Apache-2.0 | Primary multi-provider transport. Default rustls → reqwest 0.13.4 + rustls 0.23.42 + aws-lc-rs 1.17.3 (verified via `cargo tree -i aws-lc-rs` — `ring` absent). Covers 30+ providers incl. zai, bigmodel, moonshot, ollama_cloud, vertex, bedrock, open_router, github_copilot. Used behind the project-owned `ProviderTransport` trait. |
| `async-openai` | 0.41.1 | MIT | OpenAI-native escape hatch (responses / audio / realtime). Default rustls. `rustls-no-provider` feature for BYO crypto. Feature-gated `provider-openai-escape`. |
| `claude-api` | 0.5.3 | MIT OR Apache-2.0 | Anthropic-native fallback when genai lacks an Anthropic feature. Brings its own reqwest 0.12.28 (a known divergence; isolated behind the `ProviderTransport` trait). Includes `claude-api-test` cassette infra. Feature-gated `provider-anthropic-escape`. |
| `gemini-rust` | 2.0.0 (2026-07-10) | MIT | Gemini-native fallback for the new Gemini Interactions API + legacy generateContent. Feature-gated `provider-gemini-escape`. Post-MVP. |
| `secrecy` | 0.10.3 | Apache-2.0 OR MIT | `SecretString` in-memory wrapper, `ExposeSecret`, drop-zeroize. No serde derive — custom `Serialize` returns `"REDACTED"`. |
| `zeroize` | 1.9.0 | Apache-2.0 OR MIT | Drop-zeroize backend for `secrecy`. |
| `keyring` | 4.1.5 (2026-07-14) | MIT OR Apache-2.0 | OS keyring (macOS Keychain / Windows Credential Manager / Linux Secret Service) behind optional `secrets-keyring` Cargo feature. MSRV 1.88.0. |

**Rejected** (all license-OK per Arbitraitor conventions but rejected for substantive reasons):
- `rig-core 0.40.0` — README: "Here be dragons! … breaking changes." Overlaps with §9.19.1 domain catalog. Reference only.
- `llm` (rustformers, 0.1.1 crates.io) — ARCHIVED upstream.
- `llm` (graniet, 1.3.8) — author README self-declares unmaintained.
- `async-anthropic 0.6.0` — stale since 2025-05-03; `claude-api` supersedes.
- `models-dev 0.1.1` — insufficient coverage for spec §10.4 requirements; in-house client preferred.

---

## 5. HTTP, TLS, and SSRF posture

### 5.1 HTTP client

- `reqwest` 0.13.4 with rustls default backend. No `native-tls` anywhere.
- `hyper` 1.11 used directly for the daemon-internal Unix socket RPC.

### 5.2 TLS

- TLS 1.2 and 1.3 only. Certificate and hostname validation mandatory.
- Crypto provider: `aws-lc-rs` 1.17.3 (FIPS-trackable, no OpenSSL dependency).
- No user-facing "ignore all TLS errors" shortcut. Insecure modes require explicit policy.

### 5.3 SSRF and DNS

- Network enforcement is owned by Arbitraitor (`arbitraitor-fetch`, `arbitraitor-policy`; see spec §9.12, §16.4). Orchestraitor submits a `PlanContext` that includes `network_isolated` and never performs any worker-side DNS/connection policy in lieu of Arbitraitor.

---

## 6. Git and workspace control

### 6.1 Library choice

`gix` 0.85.0 (gitoxide), MIT OR Apache-2.0. Pure Rust with no C toolchain and no vendored libgit2. The `gix::open_opts()` API combined with `bail_if_untrusted(true)` refuses to execute user-level `core.fsmonitor`, hooks, or other untrusted filesystem side effects. This is the safer choice for controller-owned Git metadata isolation.

`git2` (libgit2) is rejected because of the C dependency surface and the patched OpenSSL path it pulls in on Linux.

### 6.2 Arbitraitor-managed workspace projection (spec §9.4.2)

The workspace projection (synthetic `/workspace` filesystem, path confinement, per-principal scopes, transactional overlays, mutation enforcement) is **Arbitraitor-owned and implemented**. Orchestraitor does not implement any projection layer. The `orchestraitor-arbitraitor-client` crate provides selection + activation + conformance-testing + backend-reporting calls to Arbitraitor. Three backends (`projected-vfs`, `native-overlay`, `materialized`) are Arbitraitor-managed.

**Upstream prerequisite**: if Arbitraitor does not yet expose a workspace-projection API (the `EffectiveControls` probe exists; a projection-specific capability probe + VFS/overlay/materialized backend selection may not), Orchestraitor records the gap as an upstream Arbitraitor prerequisite (see plan's Upstream Arbitraitor prerequisites section). Until Arbitraitor implements the projection, Orchestraitor uses the `materialized` backend (which is the current §9.4 snapshot mode — a real directory the controller writes, no VFS mediation) and MUST NOT claim per-operation mediation or live attribute enforcement.

### 6.3 Snapshot workspace mode

For the default snapshot workspace mode (spec §9.4 mode 1), the controller uses `gix` to export a commit tree into disposable storage. Workers receive a worktree without `.git/`. Any history access the worker needs MUST go through typed RPC methods backed by `gix`, never through filesystem `.git/` access.

---

## 7. Sandbox primitives and platform backends

### 7.1 Linux (reference platform, MVP)

The MVP runs on Linux. `arbitraitor-sandbox` is the authoritative implementation; Orchestraitor consumes its probes and reports. Concretely:

- `arbitraitor_sandbox::compute_effective_controls(mode, platform)` returns the `EffectiveControls` matrix Orchestraitor must display and honor.
- `arbitraitor_sandbox::configure_command(&mut Command, SandboxConfig)` and `apply_sandbox(&SandboxConfig)` are called by Orchestraitor when spawning workers. These call into `landlock` 0.4.5, `rustix::process` (1.1.4, `clone3`/`unshare`/`setns`), `libseccomp-rs`, and `nix` 0.31.3.
- Workspace projection: OverlayFS (mount namespaces + upperdir/workdir for transactional change staging) OR Arbitraitor-managed userspace projection (FUSE). Backend selected via `select_workspace_projection_backend()` per spec §9.4.2 + §9.32.2.
- Polkit-backed privileged operations brokered through Arbitraitor; Orchestraitor does NOT invoke polkit directly.

### 7.2 macOS (MVP — materialized-workspace backend)

- MVP uses `materialized-workspace` backend (same `gix` snapshot as Linux — real directory, no VFS mediation). Works natively on macOS; no FSKit/FUSE dependency.
- Arbitraitor capability probe reports macOS containment state:
  - `seatbelt`/`sandbox-exec` where sufficient → `process_tree_containment = Available` or `Degraded`;
  - where no sufficient macOS backend exists → `process_tree_containment = Unavailable`; Orchestraitor fails closed for strict mode, OR offers `standard` mode with explicit degraded capability report where policy permits.
- MVP does NOT require FSKit, OverlayFS, FUSE, or `launchd`-registered privileged helper.
- NEVER claim filesystem staging captures non-filesystem changes (TCC, `defaults`, system-volume protections).
- Phase 1+ (when `projected-vfs`/`native-overlay` backends land): evaluate FSKit, APFS copy-on-write clones, prototype file watching (FSEvents), mmap, locking, atomic replacement, case behavior, Unicode normalization, LSP compatibility.

### 7.3 WSL2 (Phase 1+, not MVP)

- Linux guest operations: same as §7.1 (full Linux enforcement applies inside the WSL guest).
- `/mnt/<drive>` detection: warn about weaker permissions (no xattrs), metadata loss, 9P performance overhead, case-insensitive default (`DrvFs`).
- Three control domains clearly separated in capability report: Linux-guest, Windows-filesystem-via-mounts, Windows-host-administration (requires future Windows-native Arbitraitor broker; Orchestraitor MUST NOT claim control).

### 7.4 Windows native (future, post-WSL2)

- ProjFS (Windows Projected File System) for filesystem projection; do NOT assume it provides all required interception.
- AppContainer / Windows Sandbox / Job Object for process containment.
- Windows ACLs (NOT POSIX permissions) for filesystem authorization.
- Registry + service adapters (separate from filesystem staging).
- User-consent UI (UAC or consent dialog).
- Separate Arbitraitor crate set — NOT a thin wrapper around WSL.
- Until implemented: Windows users routed to WSL2 with explicit "Windows-native backend: not yet implemented; using WSL2 Linux guest enforcement" capability report.

### 7.5 Cross-platform conformance suite

One suite covering all platforms (spec §9.32.5 + §21.7). NOT separate per-platform suites with divergent assertions. Tests: reads/writes/truncation/rename/delete, symlinks/hardlinks, permissions/ownership/executable metadata, case sensitivity + Unicode normalization, file watching, mmap + locking, atomic replacement, helper processes, concurrent IDE edits, large repo indexing, rollback + promotion, crash recovery.

### 7.6 No coupling to single OS mechanisms

The architecture MUST NOT couple to OverlayFS, FUSE, FSKit, ProjFS, polkit, launchd, or any single OS mechanism. Platform-neutral conceptual capabilities (spec §9.32.1) are the stable interface. Actual names derived from Arbitraitor's implementation per platform.

---

## 8. Receipts and provenance

- `arbitraitor_receipt::ReceiptBuilder` builds receipts. `canonical_bytes()` produces RFC 8785 JCS bytes via `serde_json_canonicalizer` 0.3.2.
- `sign_receipt()` uses minisign-ed25519-blake2b-prehashed (Arbitraitor's algorithm). Where Orchestraitor needs to verify receipts independently, `ed25519-dalek` 3.0.0 is used with `verify_strict` to reject weak keys.
- `sigstore` 0.14.0 is an opt-in feature for cosign/Rekor provenance; it is NOT a hard dependency in the MVP build.
- in-toto Statement export (`to_intoto_statement()`) is the supported interchange format for downstream audit systems.

---

## 9. LSP, tree-sitter, and semantic intelligence

### 9.1 Tree-sitter baseline indexer

- `tree-sitter` 0.26.11 + per-language grammars (all MIT). Initial grammar set: `tree-sitter-rust` 0.24.2, `tree-sitter-typescript`, `tree-sitter-javascript`, `tree-sitter-python`, `tree-sitter-go`, `tree-sitter-bash`. Additional grammars feature-gated.
- Untrusted or third-party grammars MUST be loaded via the `wasm` feature + `wasmtime` runtime, NOT compiled into the daemon.

### 9.2 LSP

- `lsp-types` 0.97.0 for the wire types. Last release is 2024-06 (LSP 3.17 spec is stable). Monitor for an upstream fork or 0.100 bump.
- `tower-lsp` is rejected for the MVP because Orchestraitor is a client of LSPs (when wrapping a harness that launches an LSP), not a server. The crate is added later if the harness needs to expose an LSP server surface.

---

## 10. Concurrency and runtime

- Tokio `current_thread` runtime for the daemon by default. Idle RSS target ≤ 60 MiB is achievable with this runtime shape plus minimal features (`rt`, `macros`, `sync`, `time`, `net`, `process`, `io-util`, `signal`).
- For per-session worker processes, a worker pool with bounded channels. No unbounded channels anywhere (matches spec Appendix F CI gate `no_unbounded_channels`).
- `async-std` is rejected as a discontinued project.

---

## 11. Storage

- SQLite via `rusqlite` with WAL mode for transactional metadata (sessions, events, approvals, token/cost ledger, repository index pointers).
- Filesystem CAS by SHA-256 for blobs, logs, receipts, and indexes. Same layout convention as Arbitraitor `store/`.
- `redb` and `rocksdb` are rejected for the MVP because SQLite is sufficient and avoids a build-time C dependency beyond what `rusqlite`'s bundled `libsqlite3-sys` already provides.

---

## 12. Error handling and diagnostics

- `thiserror` 2 for typed errors at library boundaries (matches Arbitraitor `conventions.md`).
- `miette` at the CLI boundary for diagnostic-rich user output.
- `tracing` + `tracing-subscriber` for structured telemetry. A custom `Layer` writes the audit event store as JSON Lines (canonical bytes via `serde_json_canonicalizer`) and forwards in-process events to the TUI subscription bus.
- Match Arbitraitor rule: secrets, headers, cookies, signed URLs, approval tokens, and secret-broker payloads MUST NOT appear in error strings or traces (see Arbitraitor `conventions.md:92-98`).

---

## 13. Config

### 13.1 Layered configuration

- `figment` 0.10.19 for layered TOML (the 7-tier precedence chain in spec §9.22.2: built-in defaults → plugin defaults → global user → org/team → project → directory/domain → task/agent override → explicit CLI flag). Caveat: the crate is in maintenance mode; the plan calls for a `serde + toml` escape hatch if upstream goes dormant.
- `toml` 1.1.3 for parsing; `toml_edit` for format-preserving edits (the `orc config set` / `orc config migrate` surfaces — `toml_edit` preserves comments and formatting).
- secrets referenced via `secret://keyring/<id>` / `secret://env/<VAR>` URIs, never plaintext. Per-provider API keys resolved from the `env` field (array, per models.dev convention — `NEURALWATT_API_KEY` for Neuralwatt, `ZHIPU_API_KEY` for Z.ai/zhipuai, NOT `ZAI_API_KEY`). See tech-stack §3.1.

### 13.2 Schema validation and migration

- **Schema**: JSON Schema 2020-12 via `schemars` 0.8.x. Every config key has a `description` field surfaced by `orc config explain`. Unknown keys warn (not silently ignored). Type mismatches fail `orc config validate`.
- **Diffability**: `orc config diff` shows effective-vs-defaults; `--layer=X` isolates a layer's contribution; `--json` for machine consumption.
- **Migration**: `orc config migrate` applies forward-only migrations between Orchestraitor versions. Non-destructive; preserves comments via `toml_edit`; backups old file as `orchestraitor.toml.bak.<version>`.
- **Environment overrides**: `ORCHESTRATOR_<SECTION>__<KEY>` env vars (double-underscore separates nesting). Treated as `task/agent override` layer per spec §9.22.8.
- **CLI overrides**: `--config <key>=<value>` flags. Treated as `explicit CLI flag` layer (highest) per spec §9.22.8.

### 13.3 Named profiles

Built-in profiles (`strict`, `standard`, `fast`, `interactive`) inherit from each other via `inherits = ["<profile>"]`. Profile defaults are inserted at the layer where the profile was activated; they do not bypass the §9.22.2 precedence chain. Custom team profiles follow the same inheritance rules. See spec §9.22.5.

### 13.4 Cross-channel consistency

IDE plugins (spec §11), TUI (§9.2), CLI (§9.18), daemon (§9.1), MCP server (§9.5), and provider proxy (§10.1 Mode D) all resolve configuration through the daemon-backed `orchestraitor-core` config resolver. No integration maintains a parallel config store. Changes from `orc config set` or the TUI panel are pushed to the daemon and observed by all active integrations via the event bus (§9.17). See spec §9.22.10.

---

## 14. Workspace and crate boundaries

Matches spec §16.6 layout. Reproduced here for completeness:

```text
arbsec/orchestraitor
├── crates/orchestraitor-core              # domain types, layered config, error/slog infra
├── crates/orchestraitor-daemon            # orcd: durable supervisor, scheduler, config resolver, event owner, mcp-gateway supervisor
├── crates/orchestraitor-model             # serializable domain types; no I/O
├── crates/orchestraitor-arbitraitor-client # typed client over arbitraitor crates (NOT a security authority)
├── crates/orchestraitor-workspace         # snapshot mode, gix controller, no .git exposed
├── crates/orchestraitor-context           # tree-sitter baseline indexer, context query tools
├── crates/orchestraitor-events            # normalized event schema, audit store
├── crates/orchestraitor-adapter-api       # AgentAdapter trait (spec §10.6)
├── crates/orchestraitor-adapter-host      # adapter supervisor, multiplexing
├── crates/orchestraitor-provider-api      # ProviderTransport trait (spec §10.2)
├── crates/orchestraitor-provider-proxy   # OpenAI/Anthropic-compatible local proxy
├── crates/orchestraitor-mcp               # rmcp-based MCP gateway: project-scoped server resolution, tool namespacing, lifetime management
├── crates/orchestraitor-tui               # Ratatui+crossterm reference client
├── crates/orchestraitor-cli               # orc / orchestraitor / orcd binaries
├── crates/orchestraitor-agent-catalog     # domain+role catalog, detection heuristics, routing
├── crates/orchestraitor-cost-ledger       # per-call cost/usage ledger, subscription tracker
├── crates/orchestraitor-delivery           # spec-driven autonomous delivery: task DAG, review loop, backlog runner
├── crates/orchestraitor-testkit           # testing infrastructure (cassettes, mock daemon, fixtures, deterministic provider simulator)
├── crates/orchestraitor-workspace-hack    # hakari-managed dedup crate (autogenerated)
└── xtask/                                  # docs-check, generators
```

Boundary rules:
- No Orchestraitor crate may own security-decision logic. Crate names that would imply security ownership (`-sandbox`, `-policy`, `-network-broker`, `-secret-broker`, `-approval`, `-security-receipt`) are forbidden per spec §16.6.
- `orchestraitor-arbitraitor-client` is a typed adapter; it translates orchestration requests into Arbitraitor calls and presents results. It MUST NOT make allow/deny/containment/trust approval or promotion decisions.
- `orchestraitor-core` owns no I/O. `orchestraitor-model` owns serializable types and no I/O.

---

## 15. CI and governance parity

Orchestraitor matches Arbitraitor's pre-merge gate as closely as possible and extends it per spec §21.10. Pull-request CI MUST include: format, clippy w/ warnings denied, build supported feature combinations, unit + integration tests, documentation tests, deterministic provider tests (simulator, no live provider), protocol contract tests (cassette + event-trace diff), CI-safe adversarial E2E, dependency advisory + policy checks, license + source checks, coverage reporting, selected Miri tests (core crates), benchmark smoke tests (generous thresholds), configuration-schema validation, generated-file freshness checks. Use `cargo-nextest` for the main test suite; doc tests separately. Retries MAY identify flaky tests but MUST NOT convert flaky behavior into a passing gate.

```sh
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo check --workspace --all-targets --all-features --locked
cargo nextest run --workspace
cargo test --doc
cargo run -p xtask -- docs-check
rumdl check .
cargo deny check
cargo audit
```

Plus CodeQL on rust. All GitHub Actions pinned to SHA. `lefthook.yml` for local hooks. `deny.toml` for cargo-deny policy. Worktree-first git workflow (`git worktree add -b <type>/<slug>`) per Arbitraitor `AGENTS.md`. Conventional Commits PR titles. Squash merge. Mandatory adversarial review by a different agent before merge (§21.1). DCO via `.cog.toml`.

Scheduled CI includes: full cross-platform matrix (Linux + macOS MVP), extended Miri, fuzzing (5 min/target/platform, `cargo-fuzz`), mutation testing (`cargo-mutants`), full coverage, performance regression suite (stable baselines, pinned hardware), live-provider contract tests (manual/trusted workflow), privileged sandbox E2E (ephemeral VMs), filesystem backend conformance (§9.32.5), long-running recovery + cancellation tests, dependency vetting (`cargo-vet`). Release candidates MUST pass applicable privileged + adversarial suites before publishing.

---

## 16. Deliberately rejected initial choices

- **`async-std`** — discontinued by maintainers; use `tokio` or `smol`.
- **`tower-jsonrpc`** — does not exist on crates.io (404).
- **`serde_jcs`** — abandoned; use `serde_json_canonicalizer`.
- **`native-tls`** — pulls OS TLS stacks (OpenSSL/SChannel/Secure Transport) with higher CVE churn; rejected in favor of `rustls` + `aws-lc-rs`.
- **`ring` as a rustls crypto provider** — legacy; use `aws-lc-rs`.
- **`git2` (libgit2 binding)** — C dependency surface, patched OpenSSL path on Linux; use `gix` (pure Rust, `bail_if_untrusted()`).
- **`lsp-types` server-side crates** (`tower-lsp`) — Orchestraitor is a client of LSPs in the MVP; server surface deferred.
- **Tauri in the default build** — optional, behind a `gui` Cargo feature; default build is headless.
- **Per-brand sub-agent taxonomy** (e.g., Sisyphus, Oracle, Metis) — rejected for MVP. Domains and roles are the user-facing shape (spec §9.19.1).
- **Implementing any security primitive in Orchestraitor** — rejected. Security lives in Arbitraitor (spec §2.2). The `security` domain agent is analysis only.
- **Reusing `https://open.bigmodel.cn/api/paas/v4/`** — deprecated Zhipu branding. Use Z.ai or Neuralwatt endpoints (§3.1).
- **Inferring a provider from a hostname or model prefix** — rejected (spec §10.3). The provider protocol must be explicit in configuration.
- **Treating the default Arbitraitor MCP stdio server as providing approve/execute capabilities** — rejected as a security-critical bug. Approve/Execute tools require explicit `McpServer` construction (§2.3).
- **Publishing any Orchestraitor crate to crates.io before Arbitraitor stabilizes its API surface** — deferred until Arbitraitor tags a `v0.2` or `v1.0` release.

---

## 17. Risks requiring prototypes before commitment

- **Arbitraitor API churn.** All 28 crates are pre-1.0 with `publish = false`. Exact-rev pinning means every Arbitraitor release is a manual bump in Orchestraitor. The CI "Arbitraitor bump" job runs the test suite against the latest Arbitraitor `main` weekly; red status is informative, not blocking.
- **`figment` maintenance mode.** Plan calls for the config layer to be implemented against `figment` for now, with a `serde + toml` escape hatch designed from day one and switched to if `figment` goes dormant for >18 months.
- **`lsp-types` is in slow release cadence.** The wire spec is stable, but a fork may eventually be needed if upstream stops responding to PRs.
- **TUI startup budget ≤ 150 ms warm.** Achievable with Ratatui's double-buffered render and event-driven subscriptions, but the cost ledger and live routing panels may push it. Profile in CI per spec Appendix F.
- **Domain detection false-positives at `orc init`.** Conservative thresholds + the always-enabled `general` fallback limit blast radius; user confirmation is the backstop.

---

## 18. License compatibility quick reference

| License | Compatible with dual MIT OR Apache-2.0? (consumer + library) |
|---|:---:|
| MIT | yes — exact match |
| Apache-2.0 | yes — exact match |
| BSD-3-Clause | yes — permissive |
| ISC | yes — permissive |
| MPL-2.0 | yes with caveat — file-level copyleft; modifications to MPL files must remain MPL. No MPL crates in the MVP stack today. |
| Apache-2.0 WITH LLVM-exception | yes — permissive + patent grant |
| Unlicense / CC0 | do not introduce (public domain dedication; jurisdictional concerns) |
| GPLv2 / GPLv3 | NO — strong copyleft; incompatible |
| AGPLv3 | NO — network copyleft; incompatible |
| BUSL / SSPL | NO — source-available, not OSI |

No crate in §1 carries an incompatible license.

---

## 19. Final stack decision

The MVP ships with the §1 baseline. Every choice above is reversible because Orchestraitor isolates third-party crates behind project-owned traits:

- providers behind `ProviderTransport` (spec §10.2);
- agent adapters behind `AgentAdapter` (spec §10.6);
- Git behind the controller's typed RPC, not direct `gix` calls in worker code;
- MCP behind `rmcp` macros and an internal server trait.

Pre-MVP release blockers:
1. Arbitraitor dependency pins resolved either via git-rev (now) or semver git tags (when Arbitraitor publishes them).
2. Phase 0 kill criteria (spec §4.3) demonstrated on Linux for the Neuralwatt + GLM-5.2 path.
3. CI green for the parity gate in §15.
4. `docs/spec/spec.md` and `docs/spec/tech-stack.md` always in sync (xtask `docs-check` validates).
