# Orchestraitor specification: Contracts, data, and quality

### 9.22 Customization and configuration

The harness is opinionated by default, customizable by design, and never mysterious about which configuration is active. All workflow behavior has sane defaults but is easy to inspect, change, disable, or override.

#### 9.22.1 What must be configurable (never hardcoded)

The following MUST NOT be hardcoded constants in any Orchestraitor crate:

- task taxonomies and decomposition rules (domains, roles, agents — see §9.19.1);
- token thresholds and cost caps (see §9.19.4-§9.19.6);
- review limits, verification commands, and QA gates;
- autonomy rules and per-session permission boundaries;
- model routing rules and the routing precedence (see §9.19.2);
- artifact retention, log retention, and lifecycle behavior;
- context compiler policies, index budgets, and omission thresholds;
- normalization rules (format-on-write, safe/unsafe fix classification, max passes — see §9.5);
- workspace mode defaults (§9.4) and security mode defaults (§14).

Built-in defaults are shipped as data (TOML tables or feature-gated const data), not as compile-time constants in business logic. This allows every default to be overridden by a higher layer without code changes.

#### 9.22.2 Layered configuration precedence

Configurable values resolve through this precedence chain, evaluated in order; the first match wins. Lower-numbered layers set defaults; higher-numbered layers tighten or override. For non-security values, weakening a parent layer is permitted. For security-sensitive values, weakening MUST be explicit, auditable, and limited to options Arbitraitor supports (see §9.22.9).

```text
built-in defaults
  -> global user config
  -> organization/team policy
  -> project config
  -> directory/domain config
  -> task/agent override
  -> explicit CLI flag
```

This precedence supersedes the §9.19.2 routing-specific precedence for all non-routing config keys. Model routing (§9.19.2) resolves through this same chain with routing-specific sub-keys at each layer.

#### 9.22.3 Resolved-value inspection

Every configurable value MUST be inspectable with its source layer. The CLI exposes:

```sh
orc config get <key>           # prints the resolved value
orc config explain <key>      # prints the resolved value + source layer + which layer set it
orc config set <key> <value>  # sets at the active layer (default: project; --layer=x to target)
orc config unset <key>        # removes from the active layer (does not affect inherited defaults)
orc config validate           # schema-validates all layers; reports conflicts, gaps, and unknown keys
orc config diff                # shows effective config vs built-in defaults
orc config diff --layer=user  # shows effective config vs a specific layer's baseline
orc config migrate             # migrates config files across Orchestraitor versions
```

The `explain` output includes: the resolved value, the layer that provided it, whether it was inherited from a lower layer or set explicitly, and any profile (see §9.22.5) that contributed to the resolution. IDE, TUI, CLI, daemon, MCP, and proxy integrations all resolve through the same daemon-backed config resolver, so `orc config get` and the TUI config panel ALWAYS agree.

#### 9.22.4 No hardcoded task taxonomy or process rules

Built-in domains, roles, verification commands, review thresholds, context policies, and automation levels are shipped as default TOML data — not as compiled-in logic. Users MUST be able to replace:

- built-in domains (§9.19.1) with custom domains;
- task decomposition rules;
- verification command registries (which `cargo` / `npm` / `make` commands constitute "pass" or "fail");
- model routes (§9.19.2) at every layer;
- review thresholds (how many reviewers, what severity blocks merge);
- context policies (token budgets, omission rules, index scope — §9.15);
- automation levels (which tasks are fully autonomous vs prompt-required).

#### 9.22.5 Named profiles

A profile is a named, reusable configuration fragment that inherits from other profiles. Built-in profiles:

```toml
[profiles.strict]
inherits = ["standard"]
security_mode = "strict"            # spec §14.1
workspace_mode = "snapshot"         # spec §9.4 mode 1
shell_mode = "strict"               # spec §9.5
format_on_write = true
max_passes = 1

[profiles.standard]                 # the default profile
security_mode = "standard"          # spec §14.2
workspace_mode = "snapshot"
shell_mode = "standard"
format_on_write = true
max_passes = 2

[profiles.fast]
inherits = ["standard"]
security_mode = "compatible"        # spec §14.3
workspace_mode = "brokered_worktree"
format_on_write = false             # skip normalization for speed
context_profile = "aggressive"     # more aggressive context compaction

[profiles.interactive]
inherits = ["standard"]
auto_approve_repeated_identical = true
max_repeated_identical = 3
session_token_cap = 500_000
```

Custom team profiles inherit from any built-in or custom profile. A profile contributions is applied at the layer it was declared in; it does not bypass the precedence chain.

#### 9.22.6 `orc init` proposes, never locks

`orc init` (§9.18, §9.20) MUST detect and propose configuration. It MUST NOT silently lock the user into its proposal:

- it shows what was detected and what remains uncertain;
- it writes a `.orchestraitor/orchestraitor.toml` with `# Proposed by orc init` comments on each line;
- the user can accept, amend, or reject each proposal;
- nothing proposed by `orc init` is applied to security-sensitive settings without explicit confirmation;
- `orc init --dry-run` shows what would be written without writing.

#### 9.22.7 Plugin contributions

Plugins (see §12) MAY contribute configuration schemas and defaults. Plugin defaults are inserted as a layer BETWEEN `built-in defaults` and `global user config` — they NEVER override explicit user or project settings. A plugin that attempts to override a higher layer's setting MUST be rejected at `orc config validate` time with the conflict reported.

```text
built-in defaults
  -> plugin defaults (inserted here; never overrides user/project)
  -> global user config
  -> organization/team policy
  -> project config
  -> directory/domain config
  -> task/agent override
  -> explicit CLI flag
```

#### 9.22.8 Schema validation, diffability, migration

All configuration MUST be:

- **Schema-validated**: every config file (built-in defaults, user, org, project, directory, plugin) is validated against a JSON Schema 2020-12 contract maintained in `orchestraitor-core`. Unknown keys are warned, not silently ignored. Type mismatches fail `orc config validate`.
- **Documented**: every schema key has a `description` field; `orc config explain <key>` surfaces it.
- **Diffable**: `orc config diff` shows effective-vs-defaults; `orc config diff --layer=X` isolates a layer's contribution. Diff output is machine-readable (`--json`).
- **Migratable**: when schema changes between Orchestraitor versions, `orc config migrate` applies forward-only migrations with a backup of the old file. Migration is non-destructive; it preserves comments via `toml_edit` (see tech-stack §13).
- **Overridable via environment**: any key can be set via `ORCHESTRATOR_<SECTION>__<KEY>` env vars (double-underscore separates nesting). Env values are treated as `task/agent override` layer.
- **Overridable via CLI**: `--config <key>=<value>` flags are treated as `explicit CLI flag` layer (highest).

#### 9.22.8a Conflict resolution and ceilings

Layers do NOT silently override each other; conflicts MUST resolve via explicit policy:

- **Ambiguous conflicts** (two layers specify the same key with different values and no precedence rule distinguishes them) MUST be rejected at `orc config validate` time rather than silently chosen. The validator names both sources and refuses to start the daemon.
- **Non-bypassable Arbitraitor invariants** are NOT part of the precedence chain. They are absolute floors enforced by Arbitraitor's capability reports at runtime; no Orchestraitor config can relax them. See §9.22.9.
- **Organization/team policy ceilings** act as a separate ceiling layer applied AFTER precedence resolution. A config value that exceeds the org ceiling MUST be clamped to the ceiling and the user-facing value reported as `[clamped by organization policy]` in `orc config explain`. Org ceilings cannot be tightened by an auditable override path within Orchestraitor — that path lives in Arbitraitor (§9.22.9).

#### 9.22.8b Signed team policies and schema versions

The `organization/team policy` layer MAY be cryptographically signed (minisign or cosign, per Arbitraitor `arbitraitor-receipt` signing API). When a signed policy file is present:

- The harness verifies its signature against the configured trust root before applying it.
- Unsigned policy files are accepted only when explicitly enabled by an audited override (treated as security-sensitive per §9.22.9).
- Schema versions are stamped; migrations apply to signed files in-place with a `.bak.<version>` backup (signed files keep their signature file alongside the migrated TOML).
- Deprecation warnings: when a config key is deprecated, `orc config validate` warns; when it is removed entirely, validation fails with the removal version cited. Rollback across schema versions is supported via `orc config migrate --undo` (forward-only by default; `--undo` reverts the most recent migration).

#### 9.22.9 Security controls remain Arbitraitor-owned

Orchestraitor MAY expose configurable security profiles (mapped to spec §14 security modes), but it MUST NOT:

- bypass Arbitraitor invariants (§2.2);
- implement separate security enforcement;
- silently weaken a security control;

Any weakening of security MUST be:

- **explicit**: the user must type a confirmation, not click through a default;
- **visible**: the session indicator (TUI status bar, CLI output, `orc status`) shows the weakened mode;
- **auditable**: the weakening is recorded in the event store (§9.17) and the session receipt (§9.9);
- **limited to options supported by Arbitraitor**: if Arbitraitor does not support a weaker mode, Orchestraitor MUST NOT invent one.

#### 9.22.10 Cross-channel consistency

IDE plugins (§11), TUI (§9.2), CLI (§9.18), daemon (§9.1), MCP server (§9.5, §9.18), and provider proxy (§10.1 Mode D) all resolve configuration through the daemon-backed `orchestraitor-core` config resolver. No integration maintains a parallel config store. When the user changes a value via `orc config set` or the TUI config panel, the new value is pushed to the daemon and every active integration observes the update via the event bus (§9.17).

#### 9.22.11 Design principle

> Opinionated by default, customizable by design, and never mysterious about which configuration is active.

---

## 13. Performance and footprint requirements

Performance is a product requirement, not an implementation detail.

### 13.1 Baseline budgets

Initial targets on a modern Linux desktop:

| Component | Idle RSS target | Idle CPU target | Startup target |
|---|---:|---:|---:|
| Core daemon, no indexed repo | <= 60 MB | effectively 0% | <= 100 ms |
| TUI | <= 35 MB | effectively 0% | <= 150 ms |
| VS Code extension incremental overhead | <= 25 MB extension-host memory | effectively 0% | no visible startup delay |
| JetBrains plugin incremental overhead | <= 40 MB JVM heap | effectively 0% | no visible startup delay |
| Context worker, idle after index | <= 100 MB plus bounded index cache | effectively 0% | lazy |
| Per-session adapter overhead excluding harness | <= 25 MB | effectively 0% when waiting | <= 100 ms |

These are targets, not promises. They should be refined with prototypes and published benchmark methodology.

### 13.2 Repository indexing budgets

For a one-million-line mixed-language repository:

- initial baseline index under 30 seconds on a modern desktop;
- incremental update under 300 ms for a typical saved file;
- bounded memory through on-disk content-addressed storage;
- no full re-index on branch switch when blobs already exist;
- indexing concurrency capped by policy;
- pause or reduce priority on battery;
- optional language-specific analysis.

### 13.3 UI performance

- 60 fps is not required for static screens, but typing and scrolling must feel immediate.
- Input-to-render p95 under 16 ms in normal TUI operation.
- Diff views virtualized.
- Terminal output capped and spooled to disk.
- No continuous polling when event-driven subscriptions are possible.
- Animations disabled by default or extremely cheap.
- Background sessions represented by compact state, not full retained render trees.

#### 13.3.1 Startup progress feedback

Any startup operation (or shutdown operation) that takes longer than **200 ms** MUST emit user-visible progress feedback before, during, and after. This includes:

- models.dev catalog refresh (live fetch + cache validation);
- Arbitraitor capability probe (`compute_effective_controls`);
- workspace snapshot creation for the active session;
- MCP server manifest loading + trust verification;
- plugin scan + admission;
- ACP/MCP transport handshake (when an IDE client is connecting);
- secret-store init (keyring unlock);
- index load for the active repo (when not already cached);
- update check;
- configuration layer merge + validation.

The harness MUST NOT perform these silently behind a blank screen — that pattern (visible in some harnesses today) is unacceptable UX. The TUI shows a single-line status banner naming the operation, an indeterminate progress indicator, elapsed time, and a count ("loading 4 of 12 MCP servers"). Operations taking longer than 1 s MUST offer an explanation and allow the user to skip non-critical ones (with consequences shown — e.g., "skip models.dev refresh → using bundled snapshot, prices may be stale").

A CI gate (spec Appendix F) records startup duration of every operation; operations exceeding their budget-by-default flag the gate. The default startup budget for the daemon (≤100 ms cold, spec Appendix F) DOES NOT include these async-refresh operations — they run after the daemon is ready, not on the daemon's critical startup path. The TUI's startup budget (≤150 ms warm, spec Appendix F) DOES include the time to render the progress banner.

User experience and developer experience are front-of-mind for all UI work. Security is front-of-mind for all backend operations. The two priorities are never in conflict — when they appear to be, the UI surfaces explanation rather than concealment, and the backend refuses silent weakening rather than guessing.

### 13.4 Model-path performance

Measure separately:

- harness startup;
- provider first-token latency;
- context compilation;
- tool dispatch;
- sandbox launch;
- command runtime;
- event normalization;
- UI rendering.

Do not claim end-to-end speed improvements based on native startup alone.

### 13.5 Token efficiency budgets

Required telemetry:

- raw candidate context tokens;
- selected context tokens;
- prompt-cache eligible tokens;
- repeated tokens avoided;
- tool output before and after compaction;
- context expansion requests;
- provider-reported input/output/reasoning tokens;
- estimated monetary cost;
- task success and human correction count;
- per-call model-routing decision (precedence step matched; see §9.19.2);
- per-call workers spawned, per-worker cost attribution (see §9.19.4);

Initial targets:

- 30% median input-token reduction against direct harness baseline;
- 50% reduction on large-repository navigation tasks;
- less than 3% relative task-success regression;
- no hidden truncation of security-relevant findings;
- context compiler overhead below 300 ms p95 for cached repositories.
- normalization orchestration overhead below 25 ms p95, excluding formatter process time.
- compact normalization patch generation below 15 ms p95 for files under 1 MiB.
- no full-repository scan after ordinary agent writes when a changed-layer journal is available.

### 13.6 Build and binary size

- Feature-gated optional backends
- Avoid bundling all IDE or GUI assets in the daemon
- Thin LTO for release
- Strip symbols in distribution builds
- Separate debug symbols
- Prefer rustls over platform-heavy TLS stacks when appropriate
- Avoid duplicate async runtimes
- Avoid embedding language servers
- Download optional analyzers on demand through inspected artifacts

---

## 17. API and protocol outline

### 17.1 Daemon API domains

- `repositories`
- `sessions`
- `agents` (catalog CRUD, spawn, per-agent cost rollups)
- `agent_catalog` (domain/role registry, detection results)
- `providers`
- `provider_proxy`
- `model_routing` (live routing decisions + override rules; see §9.19.2)
- `costs` (per-agent / per-domain / per-role / per-provider / per-subscription totals; see §9.19.4)
- `usage` (custom query surface for the TUI; both API spend and subscription utilization, with the `measured` / `estimated` / `user-configured` label)
- `subscriptions` (CRUD for the optional subscription metadata in §9.19.5)
- `budgets` (per-scope budgets and caps; see §9.19.6)
- `backlog` (task DAG, autonomous delivery state, review loops; see §9.33)
- `integrations`
- `workspaces`
- `context`
- `tools`
- `plans`
- `approvals`
- `arbitraitor` (the integration boundary; calls resolve to arbitraitor_* crates via the path/git dep, NOT a separate security authority)
- `policies` (Arbitraitor-backed)
- `sandbox` (Arbitraitor-backed; effective-control reports from `arbitraitor_sandbox::EffectiveControls` / `arbitraitor_exec::EffectiveControls`)
- `network` (Arbitraitor-backed)
- `secrets` (Arbitraitor-backed)
- `changes`
- `promotions`
- `git`
- `events`
- `receipts`
- `plugins`
- `health`
- `metrics`

### 17.2 Process topology, MCP gateway, and project-scoped isolation

> **Subject to prototype validation.** The topology below is the recommended starting architecture. The final default MUST be validated through ADRs + measured prototypes (§17.2.7) before being permanently resolved.

#### 17.2.1 Recommended starting topology

```text
CLI / TUI / GUI / IDE plugin        # thin clients — crash-safe; never the runtime owner
    |
    v
orcd (durable supervisor)            # scheduler, config resolver, event owner
  +-- mcp-gateway (supervised proc)  # mediates MCP protocol; resolves project-specific server sets
  +-- worker process [agent A]       # separate process per active agent/subagent attempt
  +-- worker process [agent B]       # isolated; failure does not terminate sibling work
  +-- mcp-server [local stdio #1]    # Arbitraitor-controlled sandboxed process
  +-- mcp-server [local stdio #2]    # Arbitraitor-controlled sandboxed process
```

- CLI, TUI, GUI, and IDE plugins are **thin clients**. They connect to `orcd` and MUST NOT own runtime state.
- `orcd` is the **durable supervisor, scheduler, configuration resolver, and event owner**. It survives client and gateway crashes.
- The MCP gateway is a **separate process supervised by `orcd`**. It mediates MCP protocol operations between clients (agents, IDEs) and the resolved set of local + remote MCP servers. It is NOT a security boundary — it routes and namespaces, it does not enforce.
- Each active agent or subagent attempt normally runs in a **separate worker process**. Worker failure does not terminate unrelated work.
- Each local MCP server runs as a **separately sandboxed Arbitraitor-controlled process** (§9.18.1 fingerprinting + §9.6 containment).
- Arbitraitor exclusively owns filesystem projections, process/network containment, capabilities, approvals, enforcement, and receipts (§2.2, §9.4.2, §9.6, §9.12, §9.13, §9.9).
- UI or gateway crashes MUST NOT terminate durable work. `orcd` supervises; crashes are recovered per §9.24.

#### 17.2.2 One Orchestraitor MCP registration, project-specific resolution

The MCP gateway exposes one registration surface to clients while resolving project-specific server sets internally:

```sh
# stdio transport (for agents that spawn a subprocess)
orc mcp stdio --project auto   # auto-detects project from cwd

# HTTP transport (for IDEs and daemons)
http://127.0.0.1:<port>/mcp/projects/<project-id>/tools
http://127.0.0.1:<port>/mcp/projects/<project-id>/resources
http://127.0.0.1:<port>/mcp/projects/<project-id>/prompts
```

`--project auto` resolves the project from the current working directory's nearest `.orchestraitor/` or git root. The gateway then resolves the server set for that project from the layered configuration (§9.22) — global → organization → project → directory → task → agent layers — and exposes only that project's tools, resources, and prompts.

#### 17.2.3 Project isolation hard invariant

Servers, tools, credentials, indexes, and state MUST NOT leak between projects. Each project's MCP server set, agent context, workspace, cost ledger, and event store are isolated. The gateway enforces this by:

- resolving the project from the connection's project-id BEFORE listing tools;
- refusing cross-project tool calls (a tool registered for project A MUST NOT be callable from project B's session);
- isolating per-project MCP server processes (project A's `context7` server is a separate process from project B's `context7` server, even if both use the same binary).

#### 17.2.4 MCP server lifetime classification

Servers are classified by lifetime so `orcd` can manage them appropriately:

| Class | Lifetime | State | Example |
|---|---|---|---|
| `global-stateless` | entire `orcd` lifetime | none | a read-only reference server (e.g., a documentation fetcher) |
| `global-authenticated` | entire `orcd` lifetime | credentials only | a GitHub API server with a long-lived token |
| `project-readonly` | project session | read-only index | a tree-sitter context index for the project |
| `project-stateful` | project session | read-write state | a server that maintains project-local caches |
| `session-writable` | agent session | per-session write state | a server storing per-session scratch data |
| `task-ephemeral` | single task | ephemeral | a server spawned for one tool-call batch and destroyed |

Lifetime classification is configurable per server in `.agent/mcp.toml`. Servers that don't declare a lifetime default to `project-stateful`.

#### 17.2.5 Gateway is NOT a filesystem sandbox

The MCP gateway mediates MCP protocol operations (tool calls, resource reads, prompt rendering). It does NOT mediate filesystem syscalls made directly by an MCP server process. A local MCP server that writes to the filesystem MUST be confined by an Arbitraitor-provided projected VFS, native overlay, or materialized workspace (§9.4.2) — the gateway cannot substitute for filesystem containment.

**Hard invariant**: a proxy may block MCP calls (by refusing to route a tool invocation), but it CANNOT mediate filesystem syscalls made directly by a sandboxed MCP server process. Filesystem mediation is Arbitraitor's exclusive domain (§9.4.2, §16.4).

#### 17.2.6 Tool exposure strategy: hybrid

The preferred initial tool strategy is **hybrid**:

- **Direct exposure**: built-in tools (§9.5 `fs.*`, `format.run`, `lint.run`, `check.run`, `test.run`, `task.run`) and frequently-used project tools are exposed directly by the gateway without requiring a separate MCP server round-trip. These use the Orchestraitor-owned typed tool API internally.
- **Discovery exposure**: long-tail tools (third-party MCP servers, project-specific custom tools) are discovered via the gateway's tool-list endpoint and invoked through the MCP protocol. The gateway handles namespacing, deduplication, and capability cross-checking (§9.18.1).

This avoids the latency of a separate process + JSON-RPC round-trip for every filesystem operation while preserving the extensibility of MCP for custom tools.

#### 17.2.7 Alternatives to evaluate through ADRs and prototypes

The topology above is the recommended starting point. The following alternatives MUST be evaluated through ADRs + measured prototypes before the default is permanently resolved:

1. **Gateway placement**: embedded in `orcd` (simpler, one process) vs. supervised process (crash isolation, independent restart) vs. standalone daemon (independent scaling, separate trust boundary).
2. **Gateway scope**: single global gateway (one process for all projects) vs. per-project gateway (isolation, more processes) vs. per-session gateway (maximum isolation, most overhead).
3. **Tool exposure**: transparent (all tools look like MCP tools to the client) vs. routed (built-ins use direct API, custom tools use MCP) vs. hybrid (built-ins direct, custom tools via MCP discovery).
4. **Worker model**: per-agent processes (maximum isolation) vs. worker pools (reuse, lower startup) vs. in-process tasks (lowest overhead, no isolation — rejected for untrusted work).
5. **MCP instances**: shared MCP server instances across projects (fewer processes, risk of state leakage) vs. project/session-local instances (isolation, more processes).
6. **Filesystem backend**: projected VFS (maximum mediation) vs. native overlay (kernel-native, coarser) vs. materialized workspace (simplest, no per-op mediation) — already evaluated per §9.4.2 + §9.32.2.

#### 17.2.8 Prototype validation measurements

Before finalizing defaults, measure:

- latency (tool-call p50/p95/p99, first-token latency through the gateway);
- memory (gateway RSS, per-server RSS, total process-tree RSS);
- token overhead (how much context the gateway's tool list adds to the model prompt);
- tool-selection accuracy (does the model pick the right tool when both built-in and MCP-discovered tools exist?);
- crash recovery (gateway crash → workers continue? `orcd` crash → workers orphaned but state persisted? client crash → work continues?);
- cancellation (does cancelling a gateway-mediated tool call propagate to the underlying MCP server?);
- process cleanup (on session end, are all MCP servers + worker processes reaped?);
- schema drift (§9.18.1 — does the gateway detect when a server's tool schema changes between sessions?);
- project isolation (can project A access project B's tools? can a tool from project A write to project B's workspace?);
- filesystem/indexing performance (§9.32.5 conformance suite — does the gateway's hybrid model add measurable overhead vs. direct filesystem access?).

Prototype results are recorded in an ADR. Defaults are set based on evidence, not preference.

#### 17.2.9 Hard invariants

1. **The trusted UI is never the runtime owner.** CLI/TUI/GUI/IDE are thin clients supervised by `orcd`. If the UI crashes, work continues.
2. **One worker failure MUST NOT terminate unrelated work.** Workers are separate processes; `orcd` supervises and reaps.
3. **One project MUST NEVER see another project's tools, files, credentials, or MCP state.** Project isolation is a hard boundary enforced by the gateway + per-project server processes + Arbitraitor's filesystem projection.
4. **A proxy MAY block MCP calls but CANNOT mediate filesystem syscalls made directly by an MCP server.** Filesystem containment is Arbitraitor's exclusive domain (§9.4.2, §16.4).
5. **Every degraded guarantee MUST be reported.** If the gateway falls back to a weaker tool-exposure model or an MCP server runs without full sandboxing, the capability report (§9.32.4) shows it.
6. **Security implementations missing from Arbitraitor MUST be added upstream to `arbsec/arbitraitor`.** The gateway does not implement containment, projection, policy, or enforcement — it routes and namespaces.

### 17.3 Example session request

```json
{
  "repository": "/home/user/src/example",
  "adapter": "claude-code",
  "workspace_mode": "snapshot",
  "security_mode": "standard",
  "model": null,
  "context_profile": "balanced",
  "network_profile": "development-default"
}
```

### 17.3 Example approval event

```json
{
  "type": "approval.required",
  "plan_digest": "sha256:...",
  "operation": "package.install",
  "summary": {
    "manager": "pnpm",
    "packages": ["example@1.2.3"],
    "network": ["registry.npmjs.org"],
    "lifecycle_scripts": 1,
    "writes": ["package.json", "pnpm-lock.yaml", "node_modules/**"]
  },
  "policy": {
    "decision": "prompt",
    "rule": "prompt-package-lifecycle-scripts"
  },
  "sandbox": {
    "required": ["network_broker", "ephemeral_root", "resource_limits"],
    "effective": ["network_broker", "ephemeral_root", "resource_limits"]
  }
}
```

### 17.4 Event semantics

Events are append-only and ordered per session. Cross-session total ordering is not required.

Events should contain monotonic timestamps, wall-clock timestamps, correlation IDs, parent operation IDs, and schema versions.

---

## 18. Data model

### 18.1 Repository

```rust
pub struct Repository {
    pub id: RepositoryId,
    pub canonical_path: PathBuf,
    pub identity: RepositoryIdentity,
    pub default_policy: PolicyRef,
    pub index_state: IndexState,
}
```

### 18.2 Session

```rust
pub struct Session {
    pub id: SessionId,
    pub repository_id: RepositoryId,
    pub adapter_id: AdapterId,
    pub workspace_id: WorkspaceId,
    pub security_mode: SecurityMode,
    pub policy_digest: Digest,
    pub state: SessionState,
    pub created_at: Timestamp,
}
```

### 18.3 Workspace

```rust
pub struct Workspace {
    pub id: WorkspaceId,
    pub mode: WorkspaceMode,
    pub base_commit: ObjectId,
    pub path: PathBuf,
    pub trust_state: WorkspaceTrustState,
    pub git_access: GitAccess,
}
```

### 18.4 Context receipt

```rust
pub struct ContextReceipt {
    pub request_id: ContextRequestId,
    pub task_class: TaskClass,
    pub budget_tokens: u64,
    pub candidate_tokens: u64,
    pub selected_tokens: u64,
    pub selected_items: Vec<ContextItemRef>,
    pub omitted_count: u64,
    pub index_digest: Digest,
    pub selection_policy_digest: Digest,
}
```

### 18.5 Promotion receipt

```rust
pub struct PromotionReceipt {
    pub workspace_id: WorkspaceId,
    pub source_digest: Digest,
    pub target_repository: RepositoryId,
    pub paths: Vec<PromotedPath>,
    pub findings: Vec<Finding>,
    pub approvals: Vec<ApprovalRef>,
    pub resulting_commit: Option<ObjectId>,
}
```

---

## 19. Storage architecture

Use a small embedded database for transactional metadata and content-addressed files for large objects.

Candidates:

- SQLite with WAL for metadata
- redb where appropriate
- RocksDB only if measured need justifies its footprint
- filesystem CAS for blobs, logs, receipts, and indexes

Storage classes:

- session metadata
- event log
- policy snapshots
- approvals
- receipts
- repository index
- tool output
- terminal spool
- downloaded artifacts
- workspace snapshots
- model telemetry

Retention must be configurable and security-sensitive data should have shorter defaults.

---

## 20. Observability

### 20.1 User-facing metrics

- active sessions
- daemon RSS and CPU
- per-session process usage
- provider latency
- token counts
- context reduction
- cache hit rate
- tool-call count
- approval count
- blocked operations
- network requests
- downloaded bytes
- changed files
- output classes
- sandbox control state

### 20.2 Diagnostic tracing

Use `tracing` with structured fields and bounded sampling.

Never log:

- raw credentials;
- full environment;
- sensitive file contents by default;
- complete provider prompts unless explicit debug capture is enabled;
- approval tokens;
- secret-broker payloads.

### 20.3 Reproducible benchmark suite

Publish:

- hardware;
- OS;
- filesystem;
- repository corpus;
- harness versions;
- provider models;
- warm/cold cache state;
- measurement commands;
- raw results.

Do not compare startup metrics against competitors while excluding work the proposed system performs.

### 20.4 Observability, privacy, and audit semantics

#### 20.4.1 OpenTelemetry alignment

Where practical, telemetry spans SHOULD align with OpenTelemetry GenAI semantic conventions (gen-ai.*) and MCP semantic conventions. A span field carrying `gen_ai.system` (e.g., `openai`, `anthropic`, `neuralwatt`, `z.ai`), `gen_ai.request.model`, `gen_ai.usage.input_tokens` etc. is preferred over Theatre-specific custom field names. MCP tool calls carry `mcp.server.name`, `mcp.tool.name`, `mcp.tool.call.id`.

#### 20.4.2 Default-on metadata, opt-in payloads

By default, telemetry records METADATA only:

- model id, provider id, request id, parent request id, latency, input/output tokens, retry count, error class, cost (per §9.19.4);
- tool name, tool call id, duration, status; NO tool arguments or tool results by default;
- MCP server id, transport, capability mask; NO manifest details;
- session id, agent (domain+role), routing decision.

Recording prompts, completions, tool arguments, tool results, MCP payloads, file contents, repository diffs, or terminal output is EXPLICIT OPT-IN via `[observability].record_payloads = true` (default `false`). When enabled, the recorder redacts secret-shaped substrings (Bearer tokens, `sk-`-prefixed keys, `secret://` URIs) at the redacting layer (§9.23.4).

#### 20.4.3 Local-first, exporter allowlist, full disable

- Telemetry is local-first by default. The local log sink (§9.17) is the only output by default.
- Optional exporters (OTLP HTTP/gRPC, JSON Lines file, syslog) trip via `[observability.exporters.*]` and are matched against an allowlist. An exporter not on the allowlist is refused at config validation time.
- Per-field redaction: every exporter layer applies the same redacting rules as the local sink.
- Sampling: traces default to 1:10 sampled (parent-based); metrics are always-on by default; logs are always-on by default.
- `[observability].enabled = false` disables all telemetry except the audit event store (§9.17) which is mandatory for security. The user is told what they lose by disabling.
- Retention: per-class retention is configurable (§9.28.5 defaults).

#### 20.4.4 Operational telemetry vs. auditable Arbitraitor receipts

Operational telemetry (latency, token counts, retry counts, error rates) is Orchestraitor's. Auditable receipts (verdicts, approvals, effective-controls reports, security findings, output-promotion records) are Arbitraitor's (per §9.17 spec §16). The two streams are KEPT SEPARATE even when serialized by the same exporter — auditable receipts carry a `kind = "arbitraitor.receipt"` marker and MUST not be elided by sampling. Sampling reductions apply to operational telemetry, never to Arbitraitor receipts.

#### 20.4.5 No cloud telemetry required for core functionality

Core functionality (sessions, agents, approvals, security, receipts, worker operations, model calls) MUST work fully offline with no telemetry exporter configured. Cloud telemetry is opt-in convenience, never a functional requirement.

---

## 21. Quality, security validation, benchmarking, and CI

### 21.1 Engineering workflow

Use test-first development for all behavior, with security boundaries receiving the strictest treatment. Default workflow:

```text
define behavior and security invariants
  -> write failing acceptance and negative tests
  -> implement the minimum change
  -> make tests pass
  -> refactor without changing behavior
  -> run independent adversarial review
  -> add regression fixtures for every discovered defect
```

BDD describes behavior, not a mandatory framework. Prefer clear Given/When/Then scenarios and behavior-oriented test names. Use Gherkin/Cucumber only where externally readable feature specifications add value. Tests should validate public behavior and security invariants rather than unnecessarily coupling to implementation details.

Every feature MUST define:

- expected behavior;
- trust boundary;
- abuse cases;
- failure behavior;
- negative tests;
- required Arbitraitor controls;
- observability and receipts;
- rollback behavior;
- performance budget.

Security-sensitive implementation MUST NOT be approved only by the same agent or context that produced it. Require an independent human or agent review context. Changes to privileged brokers, sandboxing, policy enforcement, capability issuance, filesystem projection, network controls, secret handling or unsafe code require human review before release.

All security primitives and enforcement remain exclusively owned by Arbitraitor. Missing capabilities MUST be implemented and tested in `arbsec/arbitraitor`, not duplicated in Orchestraitor.

### 21.2 Test layers

#### 21.2.1 Unit tests

Test pure logic: parsers, state transitions (§9.24), configuration resolution (§9.22), capability calculations, context selection (§9.15), compact output generation (§9.5), error classification. Specifically: policy merge and monotonic tightening, plan canonicalization, approval invalidation, path normalization, context ranking, token estimation, event schema, adapter parsing, output classification, secret redaction.

#### 21.2.2 Property tests

Use generated inputs for invariants such as:

- path normalization and traversal prevention;
- symlink confinement;
- policy monotonicity;
- configuration precedence (§9.22.2);
- action-plan canonicalization;
- digest and receipt stability (§9.17);
- protocol translation;
- retry classification (§9.26);
- event-stream recovery (§9.24.2);
- transaction convergence (§9.5);
- archive recursion;
- command-line quoting;
- malformed event inputs.

#### 21.2.3 Integration and contract tests

Test real crate boundaries and process interactions with isolated temporary workspaces. Cover:

- Orchestraitor to Arbitraitor integration;
- MCP and ACP contracts;
- OpenAI and Anthropic protocol façades (§10.2);
- filesystem and workspace backends (§9.4, §9.4.2);
- formatter and verification adapters (§9.5);
- provider routing (§9.19.2);
- CLI and daemon RPC;
- IDE and wrapped-harness adapters;
- sandbox filesystem denial;
- network denial;
- broker-only egress;
- secret non-exposure;
- Git metadata isolation;
- output promotion (§9.14);
- adapter cancellation;
- CLI version mismatch;
- repository reindex reuse;
- process cleanup.

#### 21.2.4 Compile-fail tests

Use compile-fail tests for APIs where invalid authority, capability combinations or unsafe usage should be impossible to express. Example: a `CapabilitySet` that cannot represent "full network + no filesystem" if the type system should prevent it. These tests live in `tests/compile-fail/` and are checked by attempting to compile a snippet that MUST fail.

#### 21.2.5 Snapshot tests

Use reviewed snapshots for structured CLI output, receipts, diagnostics, protocol events and compact patches. **Never auto-accept snapshot changes in CI** — a snapshot change in a PR MUST be reviewed and the diff acknowledged. Snapshots live in `tests/snapshots/` using `insta`.

#### 21.2.6 Fuzzing

Fuzz all untrusted parsers and protocol boundaries, including:

- provider streams (SSE chunks, malformed JSON, truncated responses);
- MCP and ACP messages;
- configuration files;
- patches and diffs;
- archive and path metadata;
- receipts;
- event logs;
- imported agent configuration.

Persist minimized regression cases in `tests/fuzz/corpus/`. Fuzzing runs on scheduled CI with fixed time budgets (default: 5 minutes per target per platform). `cargo-fuzz` for libFuzzer integration.

#### 21.2.7 Miri and sanitizers

Run Miri on suitable core crates (`orchestraitor-model`, `orchestraitor-core`, `orchestraitor-events`, `orchestraitor-cost-ledger`, `orchestraitor-agent-catalog`) and unsafe boundary code. Keep unsafe code absent from Orchestraitor where possible; unavoidable OS-specific unsafe security code belongs in isolated Arbitraitor crates with explicit safety invariants and dedicated review. Miri runs on scheduled CI, not on every PR (too slow).

#### 21.2.8 Mutation testing

Run mutation testing periodically (`cargo-mutants`) on policy-independent orchestration logic and security-critical decision tests to confirm that tests detect altered behavior rather than merely execute code. Scheduled CI only.

### 21.3 Deterministic AI provider simulator

Required CI MUST NEVER depend on a live model provider.

Implement a deterministic local provider simulator (in `orchestraitor-testkit`) supporting:

- OpenAI Responses API;
- OpenAI Chat Completions API;
- Anthropic Messages API;
- streaming and non-streaming responses;
- tool calls and parallel tool calls;
- structured output;
- usage and cost metadata (per §9.19.4);
- cancellation (per §9.26);
- retries and rate limits (429 with retry-after, 5xx);
- partial streams and disconnects;
- malformed and unsupported responses;
- configurable latency (virtual clock);
- provider capability negotiation (per §9.29).

Scenarios are defined as version-controlled fixtures in `tests/sim/fixtures/`. Use deterministic IDs, timestamps, random seeds and a controllable virtual clock.

The simulator MUST support scripted agent behavior such as:

```text
read file
  -> request edit
  -> receive formatter delta
  -> run verification
  -> react to failure
  -> request approval
  -> complete task
```

Also support adversarial model behavior:

- attempts to bypass tools;
- hidden shell requests;
- prompt injection compliance (spec §7.3);
- repeated denied operations;
- fabricated capability claims;
- attempts to access secrets or host paths;
- malformed patches;
- infinite tool loops;
- excessive context or output.

Optional live-provider contract tests MAY run manually or on trusted scheduled workflows, but MUST NOT be required for pull-request CI.

### 21.4 Adversarial sandbox E2E tests

Maintain purpose-built hostile fixture programs and repositories that attempt representative harmful operations while targeting disposable canaries rather than valuable host resources.

Test attempts to:

- read or modify files outside the workspace;
- escape through symlinks, hardlinks or path races;
- access environment secrets or inherited descriptors;
- connect to unauthorized network destinations;
- exfiltrate canary data to a controlled sink;
- spawn unauthorized helpers or background processes;
- exceed process, CPU, memory, disk or output limits;
- access devices, sockets, `/proc`, credentials or Git metadata;
- modify IDE configuration, hooks, CI or agent instructions;
- exploit package lifecycle scripts;
- misuse formatters, language servers or MCP helpers;
- falsely claim that a mutating MCP tool is read-only (§9.18.1);
- persist after cancellation;
- mutate the trusted checkout outside promotion;
- escape staged privileged-operation environments.

Adversarial fixtures (extending §21.4 above from the original spec): malicious repositories containing prompt-injected `README`, malicious `.vscode` tasks, malicious `.idea` config, Git hooks, poisoned `package.json` lifecycle scripts, malicious `build.rs`, Gradle init scripts, Python activation scripts, symlink escape attempts, terminal escape sequences, huge generated logs, fork bombs, zip bombs, localhost exfiltration attempts, DNS rebinding attempts, fake approval text, MCP tool-result injection.

Each test MUST assert:

1. the operation was denied, staged or approval-gated as expected;
2. no forbidden persistent effect occurred;
3. permitted work remained functional;
4. the correct Arbitraitor decision and receipt were produced;
5. cleanup and rollback succeeded.

Use three execution tiers:

```text
ci-safe
  Harmless adversarial fixtures on GitHub-hosted runners.

privileged-e2e
  Disposable dedicated VMs or ephemeral trusted runners for real sandbox,
  overlay, namespace, polkit and privileged-broker tests.

escape-lab
  Offline sacrificial environments for destructive or sandbox-escape research.
```

Do NOT run destructive escape payloads or untrusted pull-request code on persistent self-hosted runners. GitHub-hosted CI covers safe negative tests and compatible backend tests; privileged E2E uses ephemeral infrastructure with no production secrets, no trusted network access and reset-after-run guarantees.

### 21.5 Differential adapter tests

Record normalized event fixtures from supported harness versions. Detect incompatible CLI changes before release.

### 21.6 Token benchmarks

Use task categories: locate implementation, explain architecture, fix failing test, perform multi-file refactor, update dependency, add feature, investigate bug, review diff.

Compare: baseline harness, harness through control plane without context compiler, harness through control plane with context compiler, direct provider mode.

Measure success, tokens, latency, corrections, and changed-code quality.

### 21.7 Compatibility and conformance suite

Per spec §9.30: every supported OpenAI-compatible, Anthropic-compatible, MCP, ACP, wrapped CLI, IDE, and external-harness combination MUST carry recorded fixtures (cassettes + normalized event traces) and a conformance test that runs against the recorded combination on every CI run. Combinations are classified `supported` / `degraded` / `experimental` / `broken` and tracked in a combination matrix surfaced by `orc doctor` and the release-notes generator. Conformance is verified behaviorally during upgrades — an adapter behavior break against the recorded cassette surfaces as a failing test or a `broken` matrix entry, not a silent wrong-behavior change. Protocol fields the adapter does not interpret MUST be preserved under `unknown_protocol_fields` rather than silently dropped (§9.30.3).

### 21.8 Model and workflow regression evaluation

Per spec §9.31: repositories MAY carry an `orchestraitor.toml` evaluation block defining regression test cases for planning, editing, review, tool selection, context retrieval, and verification. The harness runs these on: model change, adapter version change, prompt-template change, routing-rule change, or explicit `orc eval` invocation. Regressions are surfaced as `regression.report` events and can gate CI. Canaries, shadow evaluation, and manual promotion of new defaults follow §9.31.3. Routing is never solely price-driven (§9.31.4).

### 21.9 Performance benchmarks

Define versioned benchmark scenarios and budgets for at least:

- CLI and daemon startup;
- idle RSS and steady-state memory;
- `orc init`;
- workspace creation;
- sandbox and filesystem projection setup;
- policy evaluation;
- built-in filesystem and search tools;
- transactional write, formatting and verification;
- synthetic filesystem overhead;
- MCP launch and request latency;
- provider proxy first-token and streaming overhead;
- context compilation and token reduction;
- repository indexing and invalidation;
- event persistence and recovery;
- checkpoint, rollback and promotion;
- concurrent agents and backpressure;
- large monorepos;
- shutdown and cleanup.

Record latency percentiles, throughput, allocations or memory, CPU, disk usage and output size where relevant.

Use:

- statistical wall-clock benchmarks for user-visible performance (via `hyperfine` or `criterion`);
- deterministic instruction-level benchmarks where supported;
- end-to-end scenario benchmarks for complete workflows;
- committed machine-readable baselines;
- explicit regression thresholds.

Do NOT use strict wall-clock regression gates on noisy shared runners. Pull requests MAY gate deterministic metrics and generous smoke thresholds. Stable performance regression gates run on pinned or dedicated hardware. Benchmark changes MUST include an explanation when baselines are intentionally updated.

Performance optimizations MUST NEVER weaken security guarantees without an explicit, auditable configuration change.

### 21.10 CI structure

Required pull-request CI:

```text
format                              # cargo fmt --check
clippy with warnings denied          # cargo clippy --workspace --all-targets --all-features -- -D warnings
build supported feature combinations # cargo check --workspace --all-targets --all-features --locked
unit and integration tests           # cargo nextest run --workspace
documentation tests                 # cargo test --doc
deterministic provider tests         # simulator-based, no live provider
protocol contract tests              # cassette + event-trace diff
CI-safe adversarial E2E             # harmless fixtures on GitHub-hosted runners
dependency advisory and policy checks # cargo deny check + cargo audit
license and source checks           # cargo deny check licenses bans sources
coverage reporting                   # cargo-llvm-cov
selected Miri tests                  # core crates only (model, core, events)
benchmark smoke tests               # generous thresholds, not strict gates
configuration-schema validation      # orc config validate against schemars JSON Schema
generated-file freshness checks     # cargo run -p xtask -- docs-check
```

Use `cargo-nextest` for the main test suite, while running documentation tests separately where necessary. Retries MAY identify flaky tests but MUST NOT convert flaky behavior into a passing quality gate.

Scheduled or manual CI:

```text
full cross-platform matrix            # Linux + macOS (MVP); WSL2 + Windows (future)
extended Miri                        # all suitable crates
fuzzing with fixed time budgets       # cargo-fuzz, 5 min/target/platform
mutation testing                     # cargo-mutants on orchestration + security tests
full coverage                        # cargo-llvm-cov full report
performance regression suite         # stable baselines on pinned hardware
live-provider contract tests         # manual or trusted scheduled workflow (not PR-required)
privileged sandbox E2E               # ephemeral VMs, no production secrets
filesystem backend conformance       # §9.32.5 cross-platform conformance suite
long-running recovery and cancellation tests # §9.24 lifecycle + crash recovery
dependency vetting                   # cargo-vet
```

Release candidates MUST pass the applicable privileged and adversarial suites before publishing.

### 21.11 Coverage and quality gates

Coverage is a diagnostic signal, not proof of correctness. Require **at least 80% overall line/region coverage** using the appropriate test level — NOT 80% unit-test coverage specifically. Do not encourage mocks and artificial unit tests around code that should be exercised through integration or process-level tests. A better rule:

- at least 80% overall line/region coverage;
- a higher changed-code threshold (e.g., 90% delta on PR);
- explicit negative and invariant tests for security-critical paths;
- unit tests for pure logic;
- integration tests for crate boundaries and third-party assumptions;
- E2E tests for the complete system;
- NO requirement that every line be covered specifically by a unit test.

You do not test third-party implementations, but you DO test your assumptions about them through contract and integration tests (§9.30 conformance suite).

Track:

- line and region coverage;
- changed-code coverage (delta on PR);
- security-invariant coverage (explicit invariant test count, not just percentage);
- adversarial scenario coverage (§21.4 test count + assertions);
- supported backend and platform coverage (§9.32 capability matrix);
- protocol and provider compatibility coverage (§9.30 conformance matrix).

Security-critical modules MUST have explicit invariant and negative-test requirements rather than relying only on a percentage threshold. A module with 100% line coverage but zero negative tests for its security invariants is not well-tested.

### 21.12 Security profiles and fine-grained configuration

Built-in profiles (extending §14 + §9.22.5):

```text
strict
  Maximum containment, deny by default, explicit approval and minimal authority.
  Maps to §14.1. Recommended initial default.

standard
  Secure defaults with project-aware trusted adapters and practical workflows.
  Maps to §14.2. orc init may recommend standard when detected tooling requires additional capabilities.

compatible
  Broader tool compatibility with visibly reduced guarantees.
  Maps to §14.3. Avoid the term 'relaxed'; 'compatible' describes the trade-off.

custom
  Fully user-defined settings within Arbitraitor invariants and organization ceilings.
  Maps to §14 combinations not covered by a built-in profile.
```

Use `strict` as the initial default or recommended selection. `orc init` MAY recommend `standard` when detected tooling requires additional capabilities, but MUST explain every difference.

All security behavior MUST be represented in a versioned, schema-validated configuration model (§9.22.8) from the initial implementation. The UI only needs to expose profiles and common controls; advanced users MAY configure fine-grained settings through files, CLI or policy bundles.

Fine-grained controls (extending §9.22.1):

- filesystem paths and access modes;
- process and executable allowlists;
- network destinations;
- secret scopes;
- resource limits (§9.27);
- MCP and plugin capabilities (§9.18.1, §12);
- approval rules (§9.9);
- workspace backend (§9.4.2);
- mutation promotion (§9.14);
- formatter and fixer authority (§9.5);
- provider data release (§9.28);
- privileged operations (§9.32.1);
- logging and retention (§20.4, §9.28.5);
- fallback and degraded behavior (§6.7, §9.29.3).

Every setting MUST be inspectable through resolved configuration and source provenance (§9.22.3 `orc config explain`). Any reduction in protection MUST be explicit, visible, auditable and included in receipts (§9.22.9). Organization policy MAY impose non-overridable ceilings (§9.22.8a). Arbitraitor invariants remain non-bypassable.

### 21.13 Security-first review gates

Every implementation or review loop MUST prioritize, in order:

1. containment and authority;
2. correctness and failure safety;
3. privacy and provenance (§9.15.1, §9.25);
4. recoverability (§9.24, §9.17.1);
5. compatibility (§9.30);
6. performance (§13);
7. convenience.

No feature is complete until its threat model, negative tests, Arbitraitor integration, capability report, failure behavior and rollback path are implemented.

### 21.14 Design principles

> Test intended behavior first, then actively test how it can be abused.

> A sandbox test passes only when the forbidden effect is absent, not merely when an error is returned.

> Safe defaults should be easy; weaker guarantees should remain possible but never accidental or invisible.

---

## Appendix F: Performance CI gates

Suggested CI gates for release builds:

```text
daemon_idle_rss_linux <= 60 MiB
tui_idle_rss_linux <= 35 MiB
daemon_idle_cpu_60s <= 0.25%
tui_startup_warm_p95 <= 150 ms
daemon_startup_warm_p95 <= 100 ms
cached_context_query_p95 <= 300 ms
event_dispatch_p95 <= 5 ms
approval_render_payload <= 64 KiB
tool_output_in_memory <= configured cap
no_unbounded_channels
no_unbounded_log_files
```

Regression thresholds should fail CI or require an explicitly documented override.
