# Orchestraitor specification: Model routing and provider integration

### 9.19 Agent catalog, domain routing, and cost ledger

This section defines the agent catalog, per-domain and per-role model routing, and the cost and subscription ledger. The catalog is the user-facing shape of sub-agent orchestration: domains describe technical areas, roles describe what an agent is doing in a turn.

> **Upgrade vs the v0.4 multi-agent stance.** Spec §10.9 stated multi-agent coordination is not a first-release priority. The v0.5 MVP tightens scope to ship a bundled domain-agent catalog with lead-plus-workers machinery underneath. The architectural invariants of §10.9 still hold: independent capability grants, merged change review, no uncontrolled shared files, per-agent token and cost budgets.

#### 9.19.1 Domains, roles, and the generic fallback

A `domain` is a technical specialty. A `role` is a phase of work. An agent invocation has both: `(domain, role, provider, model)`. Domains and roles are orthogonal; one domain can serve any role.

Built-in MVP domains (extensible through configuration and plugins):

```text
general         Required generic fallback. Every project has it.
frontend        Web frontend, styling, accessibility, browser runtimes.
backend         Server, services, APIs, persistence, message buses.
data            Pipelines, schemas, migrations, analytics, ML serving.
devops          CI/CD, infrastructure, packaging, release engineering.
testing         Test design, fixtures, property tests, regression suites.
documentation   Prose, reference, examples, README, ADRs.
security        Security analysis and guidance. Analysis only — never enforcement.
```

Built-in MVP roles (modelling what an agent is doing in a turn):

```text
planning        Producing or reviewing a work plan.
implementing    Producing or modifying code.
reviewing       Critiquing existing code or a diff.
testing         Designing or running tests.
researching     Gathering context (local codebase or external docs).
```

The `security` domain is for security-focused analysis and guidance only. It MUST NOT implement security enforcement independently. All security primitives — policy, sandboxing, approvals, provenance, command/script/package analysis, output promotion, secret brokering, receipts — MUST come from Arbitraitor (see §2.2, §9.6–§9.14, §16). Where a security gap exists for Orchestraitor's workloads, it MUST be implemented in `arbsec/arbitraitor` first (see §16.2); the `security` domain agent never substitutes for an absent Arbitraitor capability.

`orc init` enables only the domains it detected as relevant for the repository. It MUST NOT instantiate every built-in agent for every project. The `general` domain is always enabled.

#### 9.19.2 Per-domain and per-role model routing

Every agent invocation resolves `(provider, model)` through this precedence, evaluated in order; the first match wins:

```text
explicit agent override
  -> domain + role override
  -> domain default
  -> role default
  -> project default
  -> global default
```

This precedence pattern resolves `(provider, model)` through the layered configuration chain defined in §9.22.2. The routing-specific sub-keys (`agents.domains.<id>.routing.*`) live at every layer of that chain. When the resolver reaches the bottom without a match, the `general` domain's project-or-global default is used. Match resolution MUST be deterministic and recorded in the per-call event.

The security-precedence analogy to `arbitraitor_policy::PolicyEngine::merge_layers` (§9.8) applies only to security-sensitive keys: weakening a security default requires the explicit, audited override path described in §9.22.9. Non-security routing choices (e.g., "use GLM-5.2-flash for the `researching` role") may be weakened freely at any higher layer.

Routing is responsibility of the control plane, not the worker. A worker never selects its own model. The resolved `(provider, model, routing_reason)` is part of the per-call event.

The following sub-resolution happens WITHIN each config layer's `[agents.domains.<id>.routing]` block — it does not compete with the §9.22.2 general precedence chain. It determines which sub-key wins when a layer provides routing config for a domain:

```text
explicit agent override
  -> domain + role override
  -> domain default
  -> role default
  -> profiles.* inheritance chain
  -> project default
  -> global default
```

This sub-resolution feeds §9.22.2: the general chain picks the config layer; the routing sub-resolution above picks the specific `(provider, model)` within that layer.

#### 9.19.3 Agent manifest

Each domain agent declares a manifest (a typed analog of the adapter manifest in Appendix D):

```toml
id = "frontend"
name = "Frontend Engineer"
version = 1

[routing]
domain_default_provider = "neuralwatt"
domain_default_model = "glm-5.2"
# Optional overrides:
# [routing.role_default]
# implementing = { provider = "neuralwatt", model = "glm-5.2" }
# researching   = { provider = "neuralwatt", model = "glm-5.2-flash" }

[capabilities]
# These are request shapes, not grants. Arbitraitor still owns enforcement.
filesystem = "read_write"            # requested filesystem access shape
network    = "brokered"               # brokered | none | full
shell      = "mediated"               # mediated | strict | host
prompt_tools = true                   # may use MCP prompt tools

[scheduling]
weight_class_hint = "balanced"        # advisory; routing policy may override
isolated_workspace_per_spawn = true   # default for sub-agents

[budgets]
per_session_token_cap = 250_000
per_session_cost_cap  = "USD 5.00"     # soft cap; warnings surface in TUI
```

The Orchestraitor side of the budget declares intent. Arbitraitor's `CapabilitySet` and resource limits are the authoritative grant.

#### 9.19.4 Cost and subscription ledger

The control plane owns a per-call cost and usage ledger. The ledger is the source of truth for the TUI cost panels and for any external reporting.

Ledger entry attributes (per call):

- model, provider, agent (domain id), role, project, session, repository
- input_tokens, output_tokens, reasoning_tokens (where the provider reports them), cache_read_tokens, cache_write_tokens
- request_count
- request_id, parent_request_id
- started_at, completed_at, wall_ms
- monetary_cost_measured, monetary_cost_estimated, monetary_cost_basis
- subscription_attribution_id (link to subscription ledger if applicable)
- routing_decision (precedence step that matched; see §9.19.2)

Two ledger categories are kept SEPARATE, not merged:

1. **API spend** — actual metered cost where reliable pricing and usage data are available. Reported per call and rolled up by (agent, domain, role, session, day, month, project, provider).
2. **Subscription utilization** — usage against flat-rate subscriptions (Neuralwatt, Z.ai, OpenAI, Anthropic, JetBrains AI, GitHub Copilot, etc.) where the user has a subscription and the provider may or may not expose per-call cost. Rolled up the same way.

Subscription utilization MUST be clearly labelled as one of:

- `measured` — provider exposes enough usage telemetry to know exactly how much of the quota was consumed.
- `estimated` — partial telemetry is available; the rest is inferred from call counts and assumed token sizes.
- `user-configured` — the user supplied the quota manually; Orchestraitor tracks calls against the configured cap.

Orchestraitor MUST NOT invent precise monetary costs for flat-rate subscriptions when the provider does not expose enough information. A flat-rate subscription that consumed X% of its quota is shown as that percentage of the user-supplied monthly price ONLY when the user supplied one; otherwise it is shown as utilization.

#### 9.19.5 Optional subscription metadata and caps

The user may enter optional subscription metadata, which the ledger uses for budgeting and utilization display:

```toml
[[subscriptions]]
id = "neuralwatt-monthly"
provider = "neuralwatt"
billing_period = "monthly"            # daily | weekly | monthly | annual | custom
monthly_price_usd = 49.0             # optional; only used if user wants USD rollups
included_tokens = 50_000_000         # optional; sets a utilization denominator
soft_cap_tokens   = 45_000_000      # optional; TUI warns when crossed
hard_cap_tokens   = 50_000_000       # optional; routing falls back to alternative provider
active_time_cap_minutes_per_day = 480 # optional
reset_at = "monthly:1"              # ISO weekday, day-of-month, or custom
```

`hard_cap` violations trigger the configured fallback policy (default: refuse to spawn and surface a TUI warning with the fallback options). `soft_cap` violations only surface warnings.

#### 9.19.6 Budget scopes

Configurable budget scope (insertable at any of: organization, user, project, session, domain, agent):

- monthly budget (USD or the user's reporting currency)
- monthly cap (`soft`, `hard`)
- per-day budget
- per-session token cap
- per-agent token cap
- per-session cost cap

Caps are enforced by the control plane before provider invocation; routing fallback decisions are logged.

#### 9.19.7 Surface in the TUI and via the daemon API

The TUI renders:
- active agents per session (domain, role, provider, model, last cost)
- per-agent cost rollup, per-domain rollup, per-repo rollup, per-provider rollup
- subscription utilization meters with the `measured` / `estimated` / `user-configured` label
- live per-call model-routing events (which precedence step matched)
- soft-cap and hard-cap warnings

The daemon exposes the ledger via the `agents`, `costs`, `usage`, and `model_routing` API domains (see §17.1).

### 9.28 Data-governance-aware routing

#### 9.28.1 Project data classification

The controller maintains per-project data classification referenced via file-path globs and content-pattern heuristics:

```toml
[data_classification]
default_class = "internal"

[[data_classification.rules]]
pattern = "src/**/*.py"
class = "internal"

[[data_classification.rules]]
pattern = "**/secrets/**"
class = "restricted"

[[data_classification.rules]]
pattern = "**/pubkey*.pem"
class = "public"
```

Classes: `public` (any provider), `internal` (default — per-policy providers), `confidential` (approved providers only, region-constrained), `restricted` (local-only providers only, or redaction required).

#### 9.28.2 Routing restrictions on classified data

When a tool or context operation prepares to send classified content to a provider:

- `internal` content: sends through any configured provider (default policy);
- `confidential` content: sends ONLY through providers explicitly approved in `[data_governance.confidential_providers]`; default empty (block);
- `restricted` content: NEVER sent to a remote provider; either redact (replace with `[REDACTED:filename:line-range]`) or summarize locally via a local summarization step (configurable; default refuses), or block the request entirely.

When policy requires it, the harness MUST show the user what data is about to leave the machine before the provider call (a `data_egress.preview` event with the file paths, the classification, and the destination provider). The user can approve, redact, or block.

#### 9.28.3 Provider-constrained routing at the config layer

Per-provider `[data_governance]` block in `[[providers]]` constrains which classes may flow to it:

```toml
[[providers]]
id = "neuralwatt"
# ... base_url, auth ...
[data_governance]
profile = "approved"               # configured in [data_governance.profiles.approved]
allowed_classes = ["internal", "confidential"]
region = "eu-west"                  # region tag (matched against provider config)
prohibited_patterns = ["**/env.local", "**/.aws/**"]
```

#### 9.28.4 Data release enforcement belongs to Arbitraitor

Routing policy — which class can flow to which provider — is Orchestraitor's. The enforcement boundary — refusing to release restricted data to a remote network destination — is Arbitraitor's network broker (§9.12). Orchestraitor's enforcement MUST call into Arbitraitor for any refusal; it does not implement its own network-blocking layer.

#### 9.28.5 Export, deletion, retention

- `orc data export --session=<id>` produces a privacy-preserving export (per §9.17.1);
- `orc data delete --session=<id>` removes session events, receipts (with a configurable retention floor for audit compliance), and workspace snapshots;
- Retention per class is configurable; defaults are conservative (e.g., restricted content logs expire in 7 days; public logs in 90 days).

### 9.29 Provider capability verification

#### 9.29.1 Metadata ≠ proof

`models.dev`, provider docs, and `/models` endpoints are **metadata sources, not runtime proof**. They report what a model claims to do and approximate pricing; they do not prove the live endpoint will accept a tool-calling request, return cached tokens, or support structured outputs at the user's tier.

#### 9.29.2 Verification matrix

Capability is verified by combining:

1. cached catalog metadata (models.dev) — declaring what the model claims;
2. provider discovery (`GET /v1/models`) — confirming the model id is currently served;
3. runtime capability probes — small, opt-in test requests confirming streaming format, tool-call shape, structured-output shape, reasoning fields, cache accept, attachment support, cancellation behavior. Probes are billable; the user is told the cost before running them (§10.3 §5);
4. adapter knowledge — the `orchestraitor-provider-api` adapter records what feature flags it has wired (e.g., this GLM-5.2 adapter supports interleaved thinking);
5. explicit user overrides — `[providers.<id>.models.<id>.capabilities]` block overrides metadata with explicit `proven = true|false` flags.

#### 9.29.3 Recording and degradation

Every capability claim and probe MUST record: source (catalog/discovery/probe/adapter/user-override), timestamp, confidence (`verified` / `claimed` / `unverified`), and the probe request digest if any. When a required capability is unavailable, the harness MUST visibly degrade — e.g., refuse tool-calling workloads for a model that fails the tool-call probe, surface a `degraded` banner in the TUI, and refuse to silently fall back to a different feature shape.

#### 9.29.4 Bundled offline snapshot

Per `docs/spec/tech-stack.md:§4.3` and ignoring the v0.7 revision, the harness now treats the bundled snapshot as a **fallback only**, not the default delivery path. The default is a live fetch with caching; the snapshot is used when the live fetch fails after the configured retry budget. The user is told via the startup progress indicator (`orchestraitor-mcp` startup progress, see §13.3 and §15.1) when the snapshot is fallback-loaded.

### 9.30 Compatibility and conformance suite

#### 9.30.1 Recorded fixtures and conformance tests

The repository MUST maintain recorded fixtures (cassettes + event traces) for supported:

- OpenAI-compatible endpoints (Neuralwatt + Z.ai + OpenAI reference + at least one self-hosted vLLM/llama.cpp);
- Anthropic-compatible endpoints (Anthropic reference + Z.ai `api.z.ai/api/anthropic`);
- MCP versions (`rmcp 2.2` baseline);
- ACP versions (`agent-client-protocol 1.3` baseline, `1.0` + `1.x` migration against future versions);
- wrapped CLIs (Claude Code, Codex CLI, Gemini CLI, OpenCode, Pi — when adopted, Phase 1+);
- IDEs (JetBrains, VS Code, Zed — Phase 2+);
- external harnesses integration (per `docs/spec/tech-stack.md:§4` versions).

#### 9.30.2 Combination matrix

A combination is classified as `supported`, `degraded`, `experimental`, or `broken`:

- **Supported**: cassette + event trace + integration test pass;
- **Degraded**: subset works; specific features flagged unavailable (e.g., "Claude Code 1.x via `orc wrap` works but token telemetry is `provider_reported` only");
- **Experimental**: passes locally; not gated in CI;
- **Broken**: known to fail; either fix in flight or marked unsupported in `doctor`.

Conformance is verified behaviorally during upgrades, not just by reading version strings — adapter behavior is checked against the recorded cassette, and breaks during upgrade surface as `broken` rather than as silent wrong behavior.

#### 9.30.3 No silent protocol-field loss

When a provider sends a response field that the adapter does not interpret (e.g., a new SSE event type from a future OpenAI iteration), the adapter MUST preserve the raw event in the event store under `unknown_protocol_fields` rather than silently dropping it. Future adapter updates may interpret it; the user is told about unrecognized fields via `orc doctor`.

### 9.31 Model and workflow regression evaluation

#### 9.31.1 Repository-specific evaluation cases

Repositories SHOULD carry an `orchestraitor.toml` evaluation block defining cases for planning, editing, review, tool selection, context retrieval, and verification:

```toml
[[evaluations]]
id = "fix-failing-test"
description = "Given a deliberately-broken test, the agent fixes it without modifying unrelated code"
fixtures = "tests/eval/fix-failing-test/"
metrics = ["success", "tests-pass-after", "no-unrelated-diff"]
```

#### 9.31.2 Regression detection

When models, prompts, adapters, skills, or routing rules change, the harness MUST run the configured evaluations and report regressions. A regression is a metric moving in the wrong direction by more than the configured epsilon (default: 5% relative). Regressions are surfaced as a `regression.report` event; CI can gate releases on them.

#### 9.31.3 Canaries, shadow evaluation, manual promotion

- **Canaries**: new defaults (model, adapter version, routing rule) are first routed to a configurable fraction of sessions. Failures in the canary cohort roll back automatically.
- **Shadow evaluation**: a second model receives a copy of the request (without side effects) and its output is compared against the primary; metrics recorded without affecting the user.
- **Manual promotion**: a new default that passes canary + shadow is held behind `orc config set routing.defaults.experimental_model = "..."` until explicitly promoted.

#### 9.31.4 Do not route solely from advertised metadata

Routing MUST NOT be based solely on advertised metadata (models.dev or provider docs). Cost, latency observed, observed capability match, and prior task-success rates all factor in. Routing-by-price alone is rejected as a default policy.

### 9.45 Role-based model routing

The orchestration loop routes by ROLE. A role is a phase of work in the loop; the role registry maps each role to a `(provider, model)` resolution. Built-in roles:

| Role | Phase of work |
|---|---|
| `explore` | Read-only context gathering over the codebase. |
| `research` | External context gathering — documentation, upstream sources. |
| `plan` | Producing or revising a work plan. |
| `implement` | Producing or modifying code. |
| `review` | Critiquing existing code or a diff. |
| `verify` | Running and interpreting required checks. |

Users MAY define custom roles; the registry is configuration, not a hardcoded taxonomy (§9.22.4). Roles compose with the §9.19.1 `(domain, role)` catalog: a role resolves through the §9.19.2 precedence chain (`roles.<id>.routing.*` sub-keys carry the same layer semantics as `agents.domains.<id>.routing.*`), and a worker never selects its own model — the control plane resolves `(provider, model, routing_reason)` and records it in the per-call event.

**Heuristic table first.** The default router is a static heuristic table — role → `(provider, model)` entries resolved through layered configuration. No decision model is required for the loop to run, and the heuristic table remains the fallback chain when a `DecisionProvider` is configured but unavailable.

**DecisionProvider.** Decision-model-backed selection is pluggable behind a `DecisionProvider` trait: a decision model returns typed structured outputs with calibrated confidence, not string generation. A `DecisionProvider` proposes role-to-model resolutions and the campaign session's task-selection decision ([§9.35](10-orchestrator.md#935-campaign-orchestration-session-per-decision)); the heuristic table stays the default. The TypeSafe/jev "System One" model is the reference adapter shape — typed outputs, parallel sampling, calibrated probabilities (vendor claims, not verified guarantees). It is early access with no Rust SDK, and its license is not yet allowlisted (tech-stack §17); the adapter is default-off until the license is allowlisted per the dependency policy. No hard dependency on TypeSafe/jev exists anywhere in the workspace.

**Routing decision records.** Every routing resolution is persisted as a decision record: the resolved `(provider, model)`, the precedence path that produced it, the alternatives considered, and per-alternative skip reasons — including `skipped-because-quota` for subscription-exhausted candidates (§9.46). `DecisionProvider`-proposed resolutions are recorded with the same shape plus the provider's confidence. Records are replayable: given the same board state, configuration, and ledger state, a resolution is deterministic and auditable.

### 9.46 Subscription-aware routing

Budget enforcement is not only monetary (§9.19.5-§9.19.6). Providers may be subscription-backed with usage windows and limits; the router is subscription-aware:

- **Usage state.** The cost ledger (§9.19.4) tracks per-subscription usage state — `measured`, `estimated`, or `user-configured` — as the eligibility input to routing.
- **Eligibility gate.** The gate prefers subscription-backed provider instances with remaining usage and SKIPS exhausted subscriptions when an alternative provider offers a similar model. Skips are recorded in the routing decision record's alternatives[] with `skipped-because-quota` (§9.45).
- **All-exhausted stop.** When ALL subscriptions offering a suitable model are exhausted, the router stops spawning that role's work and emits a needs-human signal. It NEVER silently falls through to metered paid spend: falling through to metered spend on exhaustion is an explicit configuration choice recorded in the routing decision record, never a default.

---

### 10.2 Provider transport architecture

Provider transport and MCP are orthogonal concerns:

- A provider transport sends model requests and receives model responses.
- MCP exposes tools, resources, prompts, roots, and optional sampling.
- ACP connects an IDE client to an agent.

The harness must implement all three without conflating their authentication or trust models.

Define a small internal transport interface owned by the project:

```rust
#[async_trait]
pub trait ProviderTransport: Send + Sync {
    fn descriptor(&self) -> &ProviderDescriptor;
    async fn list_models(&self) -> Result<Vec<DiscoveredModel>>;
    async fn stream(&self, request: ModelRequest) -> Result<ModelEventStream>;
    async fn count_tokens(&self, request: TokenCountRequest) -> Result<Option<TokenCount>>;
    async fn health(&self) -> Result<ProviderHealth>;
}
```

Required first-class protocol families:

1. OpenAI Responses API
2. OpenAI Chat Completions compatibility
3. Anthropic Messages API and Anthropic-compatible endpoints
4. Google Gemini native API
5. Google Vertex AI where workplace demand justifies it
6. Local OpenAI-compatible endpoints
7. Custom provider plugins

Provider-specific features must remain representable rather than forced into a lowest-common-denominator chat schema. The internal request model needs extension fields or typed capability modules for:

- reasoning effort and thinking budgets;
- prompt caching;
- tool choice and parallel tool calls;
- structured outputs;
- multimodal inputs;
- provider-hosted tools;
- server-side conversation state;
- responses versus chat-completions semantics;
- token counting and cache usage;
- provider-specific safety settings.

The common interface should cover orchestration, while provider-specific adapters preserve advanced features.

### 10.3 Model and provider discovery

Use explicit protocol configuration plus safe endpoint probing. Do not infer a provider solely from a hostname, model prefix, or response error.

For custom endpoints, configuration identifies the protocol:

```toml
[providers.neuralwatt]
protocol = "openai-compatible"
base_url = "https://api.neuralwatt.com/v1"
api_key = "secret://keyring/neuralwatt"
discovery = "models-endpoint"

[[providers.neuralwatt.models]]
id = "glm-5.2"
metadata_source = "manual"
```

The exact endpoint is user-supplied and should not be committed with credentials. **Neuralwatt with GLM-5.2 is the initial real-world BYOK compatibility target.** Two confirmed OpenAI Chat Completions-compatible endpoints for GLM-5.2:

| Endpoint | Operator | Notes |
|---|---|---|
| `https://api.neuralwatt.com/v1` | Neuralwatt | **Initial MVP integration target.** OpenAI Chat Completions shape (`/chat/completions`), model id `glm-5.2`. |
| `https://api.z.ai/api/paas/v4/` | Z.ai | Z.ai official. OpenAI Chat Completions shape, model id `glm-5.2`. Same underlying model. |
| ~~`https://open.bigmodel.cn/api/paas/v4/`~~ | (legacy Zhipu) | **Deprecated branding** — still functional but is the original Zhipu endpoint from before the Z.ai rebrand. New configurations should use one of the two endpoints above. Orchestraitor MUST NOT ship this URL as a default. |

No first-party Rust SDK exists from Neuralwatt, Z.ai, or BigModel. Orchestraitor uses `reqwest` (default rustls backend, see [`tech-stack.md`](tech-stack.md)) against the OpenAI Chat Completions-compatible API directly; the optional `genai` crate may be used as an implementation detail behind the project-owned `ProviderTransport` trait.

Safe discovery order:

1. Explicit user/project model declarations
2. Provider model-list endpoint when available
3. models.dev metadata mapping
4. Bundled offline catalog snapshot
5. Manual unknown-model mode

Typical model listing endpoints include OpenAI-compatible `/v1/models`, Anthropic `/v1/models`, and Gemini's native models listing API. Listing results establish availability, not complete capability support.

Capability discovery should combine:

- declared protocol;
- model-list response;
- models.dev advisory metadata;
- provider documentation adapters;
- locally cached successful feature observations;
- explicit user overrides.

Avoid paid or data-bearing probe prompts by default. A tool-calling or structured-output probe may be offered explicitly and recorded as a billable capability test.

### 10.4 models.dev integration

models.dev is the recommended shared public metadata catalog, but not an execution dependency or unquestioned source of truth.

As of 2026-07-23, its documented endpoints are:

```text
https://models.dev/api.json       Provider serving details, pricing, limits, and capabilities
https://models.dev/models.json    Provider-independent facts about underlying models
https://models.dev/catalog.json   Combined provider and model metadata
```

The project is open source under the Anomaly organization, is used by OpenCode, and stores source data as reviewed TOML files. No dedicated Rust library is required. A small internal client using `reqwest` and `serde` is lower risk than adding another abstraction dependency.

Required client behavior:

- bundle a known-good catalog snapshot for offline startup;
- refresh asynchronously, never on the critical startup path;
- use HTTP cache validators when available;
- enforce download size, timeout, content type, and redirect limits;
- store the response by digest;
- validate against a locally versioned permissive schema;
- preserve unknown fields for forward compatibility;
- record retrieval time and catalog digest;
- allow explicit refresh and rollback;
- degrade to cached or bundled data on failure;
- never remove manually configured models because the catalog is missing them.

Metadata merge precedence:

```text
session override
  > project override
  > user override
  > live endpoint observations
  > models.dev
  > bundled defaults
```

models.dev data is advisory. Known failure modes include stale limits, provider-specific endpoint mismatches, models available only through OAuth products, and models listed before a user's account can access them. The harness must distinguish:

- model identity;
- provider serving identity;
- authentication method;
- endpoint protocol;
- account entitlement;
- observed capabilities.

Custom providers may explicitly map a served model to metadata for the underlying model:

```toml
[[providers.neuralwatt.models]]
id = "glm-5.2"
metadata_from = "models.dev:<confirmed-provider/model-id>"
```

Fuzzy family matching may suggest candidates but must not silently apply pricing, context limits, or capabilities from a guessed model.

### 10.5 Rust library assessment and recommendation

Research on 2026-07-23 found several actively maintained choices.

#### Recommended baseline

- **`rmcp`**: Use the official Rust MCP SDK for MCP client and server support. It has active protocol coverage, transports, OAuth, roots, sampling, tasks, subscriptions, and schema macros.
- **`genai`**: Best initial candidate for broad native provider transport. Version 0.6.5 supports OpenAI, OpenAI Responses, Anthropic, Gemini, Vertex, many compatible providers, custom endpoints, custom authentication, and model listing. It is compact enough to prototype quickly.
- **Project-owned transport traits**: Mandatory. `genai` remains an implementation behind the interface, not the public architecture.

`genai` resolves adapters partly from model-name prefixes and has fallback behavior intended for convenience. The harness should always force an explicit adapter/protocol and must not inherit heuristic provider selection in security-sensitive configuration.

#### Provider-specific fallbacks

- **`async-openai` 0.41.1**: Actively maintained, broad OpenAI API and Responses coverage, granular features, custom base URLs, bring-your-own wire types, and Tower middleware. Strong choice for the OpenAI-native adapter or compatibility escape hatch.
- **`claude-api` 0.5.3**: Recent, forward-compatible Anthropic client with Messages, models, streaming, prompt caching, tool use, token counting, and optional advanced APIs. Suitable if `genai` lacks an Anthropic feature required by the harness.
- **`gemini-rust` 2.0.0**: Recently maintained Gemini-native option. Suitable as a fallback for Gemini features not exposed through the common transport.

#### Not recommended as the foundational transport

- **`rig-core`** is actively maintained and supports many providers, but it includes agent, memory, RAG, and workflow abstractions that overlap with the product's main purpose. It is useful as a reference, optional plugin backend, or comparison target, not as the architectural core.
- Small single-provider crates with sparse maintenance should not define the public provider model.
- A single OpenAI-compatible DTO should not be used internally for Anthropic and Gemini because it loses native features and creates translation ambiguity.

#### Dependency policy

- Pin exact minor versions during the prototype.
- Put each third-party provider library behind a crate-local adapter.
- Maintain wire-level cassette tests for every provider.
- Keep a raw `reqwest` escape hatch for unsupported fields and compatibility bugs.
- Measure binary size and compile-time impact before enabling optional provider crates by default.
- Prefer feature-gated provider crates so a minimal workplace build includes only required transports.

## Appendix B: Example BYOK provider configuration

```toml
[providers.neuralwatt]
protocol = "openai-compatible"
base_url = "https://api.neuralwatt.com/v1"
auth = "secret://keyring/neuralwatt"           # preferred (OS keyring)
# auth = "secret://env/NEURALWATT_API_KEY"      # fallback (env var convention per models.dev)
# auth = "sk-..."                                 # local dev only; never commit
discovery = "models-endpoint"
request_api = "chat-completions"

[[providers.neuralwatt.models]]
id = "glm-5.2"
context_window = 1048560
max_output_tokens = 1048560
# Optional after the user confirms an exact catalog identity:
# metadata_from = "models.dev:neuralwatt/glm-5.2"
```

The other supported GLM-5.2 endpoint is Z.ai's `https://api.z.ai/api/paas/v4/`. The legacy Zhipu endpoint `https://open.bigmodel.cn/api/paas/v4/` MUST NOT be used as a default; it remains functional only for backward compatibility with existing user configurations.

Provider values shown here are illustrative except for the user-supplied model and compatibility requirements. Endpoint, limits, and catalog mapping must be discovered or configured rather than assumed.
