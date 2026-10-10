# Orchestraitor: Safety-First Coding Agent Harness and Control Plane

**Working specification**
**Status:** Research-backed product and architecture proposal, revision 0.14
**Date:** 2026-07-23
**Product:** Orchestraitor
**Tagline:** An agent harness with trust issues.
**Repository:** `arbsec/orchestraitor`
**CLI:** `orc` (canonical) and `orchestraitor` (long form)
**Daemon:** `orcd`
**Relationship to Arbitraitor:** Sibling coding-agent orchestrator and harness whose complete security implementation and enforcement boundary are provided by Arbitraitor (`arbsec/arbitraitor`).
**Companion documents:**
- [`docs/spec/tech-stack.md`](tech-stack.md) — concrete crates, versions, license compatibility, runtime dependencies, platform support, and rejected alternatives. Every dependency and architectural claim there is verified against crates.io, GitHub, or primary docs.
- Arbitraitor internal baseline (private): [arbsec/arbitraitor `docs/spec/tech-stack.md`](https://github.com/arbsec/arbitraitor/blob/main/docs/spec/tech-stack.md). Used as structural inspiration only; not assumed up to date.

> **Vocabulary note (v0.5).** Earlier revisions used conceptual type names such as `EffectiveSandboxControls`, `ActionPlan`, and `ApprovalToken` to describe the security surface. The actual Arbitraitor identifiers differ; conceptual names are preserved in this spec ONLY for narrative continuity and are explicitly marked `[CONCEPTUAL — actual type: …]` on first use. Authoritative identifiers live in [§16 Arbitraitor integration](40-arbitraitor-integration.md#16-arbitraitor-integration-and-ownership-plan) and the tech-stack document. Code and plans MUST use the real identifiers.

---

## 1. Executive summary

This document specifies Orchestraitor, a safety-first, low-footprint orchestrator, harness, and control plane for AI coding agents.

Orchestraitor's first axis is a self-improving orchestration loop. Work items live on a kanban board; a fresh manager session selects the next eligible task; a worker implements it in an isolated workspace and opens a pull request; adversarial review runs against the result; and a human gates the merge. Every state transition in the loop is tracked on the board. The loop is bounded — autonomous operation is always constrained by explicit budgets for attempts, re-plans, timeouts, concurrency, spend, and subscription usage — and it never bypasses the Arbitraitor security boundary. The system is self-hosting: Orchestraitor's own development backlog is the loop's first and continuous workload.

The expected operating scale is a single operator with one or two workspaces, at most four boards, on the order of 10² items per board, and a concurrency of two workers. The architecture optimizes for correctness, containment, and auditability at that scale, not for fleet-size throughput.

The harness is the loop's worker surface, and it is also a first-class standalone tool. It provides its own trusted TUI and optional desktop GUI while integrating with existing coding-agent harnesses such as Claude Code, Codex CLI, Gemini CLI, OpenCode, Pi, and other Agent Client Protocol (ACP) compatible agents. It also supports direct model/provider integrations where the provider exposes a suitable API or SDK.

The product is a first-class coding-agent orchestrator and harness, not merely a wrapper or security add-on. Its defensible purpose is to combine capabilities that existing tools generally provide only separately:

1. A bounded, board-tracked, self-improving orchestration loop with human-gated merges
2. A complete native agent loop plus adapters for existing harnesses
3. Enforced runtime isolation across native and wrapped agents
4. Static, plan-bound authorization before side effects
5. Transactional filesystem tools with project-aware format-on-write and safe lint fixing
6. A trusted output boundary for files that host tools may later execute
7. A provider-independent context compiler that reduces token usage
8. First-class MCP, Agent Skills, AGENTS.md, ACP, and IDE interoperability
9. Direct OpenAI, Anthropic, Gemini, and compatible endpoint support with BYOK
10. A low-overhead native control plane for TUI, GUI, IDE, and headless clients
11. Auditable execution, policy, context, normalization, and promotion receipts

A coding agent should normally run in an isolated workspace and sandbox by default. The user may explicitly weaken those guarantees, but weakening must be visible, scoped, recorded, and never silently inferred.

Arbitraitor (`arbsec/arbitraitor`) is the exclusive security subsystem and security authority for Orchestraitor. All security-related primitives, policy decisions, containment mechanisms, capability enforcement, inspection, provenance, approval binding, promotion authorization, and security receipts must be implemented in Arbitraitor. Orchestraitor owns agent orchestration, provider and harness compatibility, context optimization, developer experience, and user interfaces. If Orchestraitor requires a security capability that Arbitraitor does not yet provide, that capability must first be designed, implemented, tested, and released in the Arbitraitor GitHub project rather than duplicated inside Orchestraitor.

### 1.1 Research conclusion

Research performed on 2026-07-23 did not find an existing product that combines the complete proposed trust model, context optimization layer, native IDE integration, provider-native operation, and wrapped third-party CLI support.

Several projects overlap substantially:

- **Agent of Empires** combines a Rust TUI, web dashboard, many CLI agents, Git worktrees, and optional container sandboxing.
- **Conductor** combines a desktop control plane, multiple harnesses, isolated workspaces, diffs, checks, and pull-request workflows.
- **Rivet Sandbox Agent** normalizes multiple agent CLIs behind a lightweight Rust service intended to run inside a sandbox.
- **Agent Sandbox** provides strong container isolation, network mediation, and proxy-side secret injection for multiple agents and devcontainers.
- **Agent Workspace** and **Codexia** provide desktop interfaces around multiple CLI agents and worktrees.
- **Daytona** and **Coder** provide remote sandbox or workspace infrastructure and IDE attachment.
- **ACP** standardizes IDE-to-agent communication and already covers JetBrains IDEs, Zed, Gemini CLI, and a growing agent ecosystem.
- **Arbitraitor** already provides a policy-enforced artifact execution gate, plan-bound approval concepts, receipts, package-manager inspection, and Linux containment primitives.

The opportunity is therefore real but narrow. The product must not compete merely on "many agents in one UI" or "agents in Docker." Those categories already exist. The differentiated product must make deterministic security, output promotion, token efficiency, auditability, extensibility, and performance its central architecture.

This conclusion is a best-effort public market survey, not proof that no private, unreleased, or obscure project has implemented the same combination.

### 1.2 Product naming and command surface

The product name is **Orchestraitor**, combining **orchestrator** and **traitor**. The name reflects both sides of the product: it is a first-class coding-agent orchestrator, while its trust model assumes that agents, wrapped harnesses, repository content, tools, and generated artifacts may behave incorrectly or maliciously.

The official tagline is:

> **Orchestraitor - An agent harness with trust issues.**

The word "trust" also contains "Rust," a fitting secondary reference to the implementation language. Branding should leave that as a subtle detail rather than requiring stylized capitalization.

Product-family relationship:

```text
Arbitraitor     Sole security engine and policy-enforced gate for untrusted artifacts and operations
Orchestraitor   Coding-agent harness and control plane that delegates all security enforcement to Arbitraitor
```

Canonical naming:

```text
Organization:    arbsec
Repository:      arbsec/orchestraitor
Product:         Orchestraitor
Short name:      Orc
Primary CLI:     orc
Long-form CLI:   orchestraitor
Daemon:          orcd
User config:     ~/.config/orchestraitor/
Project config:  orchestraitor.toml
Project state:   .orchestraitor/
```

`orc` is the canonical command used throughout documentation and normal shell workflows. The long `orchestraitor` executable should remain available as an explicit alias or symlink for discoverability and to reduce ambiguity where `orc` conflicts with another installed program.

Example command surface:

```sh
orc init
orc run claude
orc run codex
orc attach
orc diff
orc models
orc history
orc checkpoint
orc restore <node>
orc branch <node>
orc compare <a> <b>
orc undo
orc redo
orc capabilities
orc capabilities --json
orc policy show
orc doctor

arb inspect ./downloaded-artifact
arb run ./approved-script
```

Internal Rust crates intended for publication should use the full `orchestraitor-*` prefix. The shorter `orc-*` prefix may be used only for private workspace modules where collision and ambiguity are controlled.

### 1.3 Positioning

> Orchestraitor should not win by having more toggles, agents or MCP servers. It should win by making existing agent workflows safer, more predictable, easier to review, and easier to adopt without forcing users to abandon the tools they already like.

---

## 2. Product thesis

### 2.1 Core thesis

The trusted component should be the control plane, not the model and not the wrapped harness.

The model may hallucinate. The harness may be compromised. Repository content may contain prompt injection. Build tools may be malicious. IDE configuration may execute code. A sandboxed process may write a file that a more privileged host component later consumes.

The control plane must therefore mediate:

- what context reaches the model;
- which tools exist;
- what each tool is allowed to do;
- which commands and artifacts may execute;
- which paths may be read or changed;
- which network destinations may be reached;
- which secrets may be used;
- which generated files may cross into trusted host state;
- which changes may become Git commits or pull requests.

### 2.3 Product identity

The product should be described as:

> Orchestraitor is a local-first, self-improving coding-agent orchestrator, harness, and control plane, secured by Arbitraitor, with provider-independent context optimization and native developer-tool integrations.

It should not lead with:

- "an OpenCode rewrite in Rust";
- "a faster Claude Code clone";
- "a multi-agent TUI";
- "Docker for coding agents";
- "an MCP security plugin."

Those descriptions collapse the product into existing categories and understate the difficult part.

### 2.4 Defensible differentiation

The product does not win by offering more agents, more toggles, or more MCP servers than existing harnesses. Those categories are already crowded. The defensible differentiation is the combination of six capabilities that existing tools generally provide only separately, if at all:

1. **Securely retrofitting existing harnesses and workflows.** Orchestraitor attaches to harnesses and provider endpoints the user already has, rather than requiring a replacement. `orc observe`, `orc wrap`, `orc connect`, and `orc proxy` form an adoption ladder where each step adds enforcement without forcing the user to abandon a tool they already like.

2. **Transactional workspace changes with review and promotion.** Every mutation is a versioned transaction: capture base generation, stage changes, detect side effects, normalize, verify, review a compact diff, and atomically promote or roll back. The trusted checkout is never corrupted by partial failure, concurrent IDE edits, or background processes. See §9.5 and §9.14 for the full transaction and promotion model.

3. **Arbitraitor-enforced security across every integration.** Policy evaluation, sandboxing, process and filesystem containment, network and secret brokering, command and package analysis, output classification, promotion authorization, and tamper-evident receipts all originate from Arbitraitor (`arbsec/arbitraitor`). Orchestraitor never implements a parallel security authority. See §2.2 for the ownership invariant and §16 for the integration plan.

4. **Project-aware formatting, verification, and compact feedback.** Format-on-write, safe lint fixes, and project-configured verification run inside the write transaction. The agent receives a compact normalization delta rather than a full file reread, reducing token usage and round trips. See §9.5 for the normalization engine and §13.5 for token efficiency budgets.

5. **Explainable context, policy, cost, and enforcement decisions.** Every context item carries provenance. Every policy decision includes a trace. Every cost is attributed per call, per agent, per domain, per subscription. Every enforcement claim is backed by Arbitraitor capability reports, not configuration flags. See §9.15 for the context compiler, §9.17 for the event and receipt store, and §9.19 for the cost ledger.

6. **Incremental migration with minimal disruption.** Setup operations support dry-run, diff, backup, rollback, and removal. `orc init` works without a provider. `orc disconnect` restores the previous configuration. Time to disable or remove Orchestraitor is under 30 seconds with no residue. See §9.18.2 for the migration and recovery UX.

Baseline coding-agent features (multi-agent TUI, many agents in one UI, provider-independent operation, worktree management, terminal session persistence) are table stakes, not differentiation. The spec does not list them as unique selling points.

---

## 3. Goals and non-goals

### 3.1 Primary goals

Orchestraitor's primary goals are orchestration and self-hosting: the product's first axis is the bounded, board-tracked, human-gated delivery loop that runs the system's own backlog. The trust-model goals the loop depends on remain primary; the harness golden path is a secondary goal (§3.2).

1. Run the self-improving delivery loop end to end: backlog → manager selection → worker → pull request → adversarial review → human-gated merge, with every state transition tracked on the kanban board.
2. Self-host the loop: Orchestraitor's own development backlog is the loop's first and continuous workload, so orchestration capabilities are dogfooded before they are generalized.
3. Keep autonomous operation bounded: explicit budgets (attempts, re-plan, timeout, concurrency, spend, and subscription usage) constrain every autonomous run, and merges and security-sensitive changes always require human review.
4. Sandbox every new session by default.
5. Create an isolated workspace for every new session by default.
6. Keep the original checkout and shared Git metadata outside the worker trust boundary.
7. Provide deterministic, explainable, reviewable policy decisions.
8. Record what was requested, permitted, enforced, executed, normalized, changed, and promoted.
9. Make extension possible without allowing extensions to silently inherit full host authority.
10. Use Arbitraitor as the exclusive implementation and authority for every security-related capability.

### 3.2 Secondary goals

The harness golden path — interactive, single-operator use of the harness — is a secondary goal: the harness is the orchestration loop's worker surface and a first-class standalone tool, but it no longer defines the product's primary axis.

- Run existing and custom coding agents with a consistent security boundary
- Allow users to bring their own provider, model, API key, subscription-backed CLI, or custom agent
- Integrate natively with JetBrains IDEs, VS Code, Zed, Neovim, and other popular development environments
- Provide a fast native TUI and an optional low-footprint GUI
- Reduce input tokens and tool round trips without materially reducing task success
- Preserve acceptable performance on large monorepos and long-running sessions
- Detect and apply the project's configured formatter automatically after agent-authored writes unless the project opts out
- Return compact normalization deltas so the agent does not need to reread files after formatting or safe fixes
- Import common agent instructions, skills, hooks, and MCP configurations while recommending vendor-neutral canonical formats
- Support JetBrains, OpenAI, Google Gemini, OpenAI-compatible, and Anthropic-compatible provider paths without coupling provider metadata to one client library
- Allow incremental adoption through a machine-friendly CLI, MCP tool gateway, managed process wrapper, and OpenAI/Anthropic-compatible local proxy
- Parallel isolated sessions
- Multi-agent coordination
- Remote workers
- CI and headless operation
- Enterprise policy layering
- Reproducible task environments
- Pull-request creation through a broker
- Local and remote model support
- Session replay and export
- Security benchmark tooling
- Shared immutable caches
- Offline mode
- Policy-as-code

### 3.3 Non-goals for the initial release

The following are explicitly out of scope for the initial MVP. Some may be revisited after the MVP proves the core trust model.

**Product scope non-goals:**

- Full native Windows support (WSL2 is the Windows path; native Windows backend is a future Arbitraitor-owned effort, see §9.32.3.4)
- Sophisticated GUI (the TUI is the first-class reference client; the GUI is optional and architecturally present but not MVP-blocking, see §9.3)
- Remote multi-user agent fleets (local-first operation is the MVP target; remote workers are a secondary goal)
- Unbounded or unbudgeted autonomous agent swarms (autonomous orchestration is in MVP scope when it is bounded by explicit budgets — attempts, re-plan, timeout, concurrency, spend, and subscription usage — board-tracked, with every state transition recorded on the kanban board, and human-gated, with merges and security-sensitive changes always human-reviewed; autonomy that bypasses the Arbitraitor security boundary and self-modification outside the reviewed loop remain out of scope, see §10.9)
- Proprietary MCP marketplace (MCP servers are untrusted principals, not products Orchestraitor hosts or sells)
- Universal synthetic-filesystem compatibility (the workspace projection is a mediation layer, not a universal filesystem emulator; some tools will detect non-standard semantics and fail, see §9.4.2)
- Broad privileged system administration (privileged operations are brokered through Arbitraitor on supported platforms; Orchestraitor does not become a general-purpose system administration tool)
- Replacing existing Dev Container, MCP, ACP, or Agent Skills standards (Orchestraitor adopts and interoperates with these standards; it does not replace them)

**Implementation non-goals:**

- Reimplementing every proprietary harness feature
- Training or fine-tuning foundation models
- Becoming a general-purpose IDE
- Replacing Git
- Proving arbitrary code safe through static analysis
- Supporting unrestricted native execution safely on every operating system
- Transparent compatibility with every terminal application
- Building a cloud service before the local trust model works
- Shipping autonomy that bypasses the Arbitraitor security boundary or self-modification outside the reviewed loop (the bounded, budgeted, board-tracked, human-gated orchestration loop is in scope; unbounded multi-agent autonomy is not)
- Implementing security primitives or security decision logic independently inside Orchestraitor

---

## 4. Adversarial product assessment

### 4.1 Reasons not to build it

The project should be killed or narrowed if the implementation becomes any of the following:

- A TUI that starts Claude, Codex, and Gemini in tmux sessions
- A worktree manager with prettier diffs
- A Docker wrapper with provider logos
- A Rust agent loop that duplicates OpenCode feature by feature
- An MCP server that asks agents to follow security rules
- A GUI that embeds third-party terminal UIs
- A context index with no measured token or quality improvement
- A plugin framework that grants arbitrary host execution
- A daemon that consumes hundreds of megabytes while idle

Those products may still be useful, but they do not justify the proposed scope.

### 4.2 Reasons the project may be worth building

The idea solves real problems if it can demonstrate:

- a wrapped harness cannot reach host credentials or the main checkout;
- unsafe output cannot become trusted host configuration without promotion;
- network credentials can be used without becoming visible to agent-controlled code;
- users can inspect the exact capability plan before approval;
- policies work across Claude, Codex, Gemini, OpenCode, Pi, and custom agents;
- the same task uses materially fewer tokens than the unmodified harness;
- the daemon and clients stay lightweight enough to remain continuously active;
- integrations do not require reimplementing every agent for every IDE;
- output from closed CLIs can be normalized without brittle screen scraping as the only mechanism.

### 4.3 Kill criteria

Before committing to a full product, a prototype should meet all of these:

1. Wrap at least Claude Code, Codex CLI, and Gemini CLI inside the same enforced worker model.
2. Demonstrate that the worker cannot access the main checkout, host `.git`, SSH keys, cloud credentials, or arbitrary loopback services.
3. Demonstrate a malicious repository configuration attack that succeeds in a conventional worktree/container setup but is blocked by output promotion.
4. Produce a normalized event stream and diff for all three harnesses.
5. Reduce median input tokens by at least 30% on a representative repository task suite without a statistically meaningful task-success regression.
6. Keep the idle daemon below 60 MB RSS on Linux and below 1% CPU under normal idle conditions.
7. Launch the TUI to an interactive state in under 150 ms on a warm filesystem, excluding external harness startup.
8. Integrate one JetBrains IDE through ACP and one VS Code extension through the daemon API.

Failure on security should kill the broad product. Failure on token reduction should narrow it to a security control plane. Failure on footprint should trigger architectural changes before adding GUI scope.

---

## 5. Existing landscape

### 5.1 Comparison matrix

Legend:

- **Yes:** central, shipping capability
- **Partial:** available but optional, narrow, or not an enforcement boundary
- **No:** not a documented core capability
- **Unknown:** not established during this research

| Project | Multi-harness | TUI/GUI | Workspaces/worktrees | Enforced sandbox | Static policy and receipts | Secret/network broker | Context/token broker | Native IDE interoperability |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| Agent of Empires | Yes | Yes | Yes | Partial, optional containers | No | Partial | No | ACP structured view, not a general IDE security layer |
| Conductor | Yes | GUI | Yes | Partial/unknown | No | Unknown | No | Product-local workflow |
| Rivet Sandbox Agent | Yes | No, API | No | Runs inside external sandbox | No | External responsibility | No | API-level integration |
| Agent Sandbox | Yes | CLI/devcontainer | Project container | Yes | Network policy, not full action-plan receipts | Yes | No | VS Code and JetBrains devcontainers |
| Agent Workspace | Yes | GUI | Yes | Unknown/partial | No | No documented broker | No | Editor-like product UI |
| Codexia | Claude/Codex | GUI | Yes | Unknown | No | No | No | Built-in editor |
| Daytona | Adapter-specific | Web/API | Sandboxes | Yes | Infrastructure policy | Basic environment/secret mechanisms | No | IDE attachment |
| Coder | Adapter-specific | Web/IDE | Remote workspaces | Yes | Workspace policy | Enterprise infrastructure controls | No | VS Code and JetBrains remote development |
| Claude Code | Claude only | CLI/desktop/IDE | Yes | Yes, platform-specific | Harness-specific permissions | Provider-specific | Context management exists | Native plugins and ACP ecosystem |
| Codex | OpenAI only | CLI/desktop/IDE | Yes | Yes, platform-specific | Harness-specific approvals | Provider-specific | Context management exists | Native integrations |
| Gemini CLI | Gemini-centric | CLI/IDE | Manual/workspace | Yes, configurable | Harness-specific approvals | Provider-specific | Context management exists | VS Code companion and ACP |
| Arbitraitor | Agent-neutral MCP | CLI | No | Linux primitives | Yes | Partial | No | MCP, not IDE client |
| Proposed system | Yes | TUI, GUI, IDE | Controller-owned isolated sessions | Yes by default | Yes, plan-bound | Yes | Yes | ACP plus native plugins |

### 5.2 Closest matches

#### Agent of Empires

Agent of Empires is the closest match to the surface-level product. It is written in Rust and provides:

- a TUI;
- a web dashboard;
- support for many coding-agent CLIs;
- Git worktrees;
- structured ACP views;
- optional Docker, Podman, and Apple Container execution;
- diff review;
- session persistence.

However, its documented sandbox is optional and disabled by default. Its model commonly gives containers full read-write access to the project and shares or injects agent credentials. It is primarily a session and workspace manager, not a static authorization, output-promotion, or token-optimization layer.

This means the proposed system must not imitate Agent of Empires' feature list and call it differentiation.

#### Conductor

Conductor explicitly defines itself as a workspace layer above Claude Code, Codex, Cursor, and OpenCode. It creates isolated workspaces and attaches chats, terminals, diffs, checks, and pull-request workflows.

It validates that users want a harness-independent workspace and review layer. It also means "one app for several harnesses and worktrees" is not novel.

The proposed difference is enforceable security boundaries, cross-platform low-footprint local operation, open extensibility, context compilation, and output trust promotion.

#### Rivet Sandbox Agent

Rivet Sandbox Agent is an important architectural comparison. It is a lightweight Rust service designed to run inside arbitrary sandboxes. It normalizes Claude Code, Codex, OpenCode, Cursor, Amp, and Pi behind a universal HTTP and event schema.

It already addresses:

- agent-specific event normalization;
- streaming;
- permission handling;
- remote sandbox control;
- static binary deployment.

It explicitly leaves Git repository management, sandbox-provider APIs, direct model wrappers, and durable storage to the consumer.

The proposed system should consider using its concepts, protocol, or code where licensing and architecture permit rather than rebuilding every adapter blindly. The remaining product layers are still substantial: trusted UI, policy, workspace control, output promotion, context compiler, IDE clients, and Arbitraitor integration.

#### Agent Sandbox

Agent Sandbox is the closest security-oriented competitor found. It provides:

- restricted repository filesystem access;
- a sidecar network proxy;
- host/path/method-level egress rules;
- iptables enforcement;
- proxy-side secret injection, so agent code does not receive the original secret;
- reproducible containers;
- persistent agent state;
- CLI and devcontainer workflows;
- VS Code and JetBrains support.

This is serious overlap and should be treated as a design reference, possible integration, or potential upstream collaboration target.

Its current product is still primarily a project-local container environment. It does not provide the proposed trusted session dashboard, normalized agent orchestration, plan-bound action approvals, output promotion, content-addressed context broker, or direct provider mode.

#### ACP

ACP already solves much of the IDE-agent protocol problem. JetBrains and Zed support it, and Gemini CLI and other agents expose ACP modes. The protocol has SDKs including Rust.

The Rust SDK is maintained at **`agentclientprotocol/rust-sdk`** (organization moved away from the previous `zed-industries/...` home; that path now redirects). The published crates are `agent-client-protocol` and `agent-client-protocol-schema`, both Apache-2.0, with companion crates `agent-client-protocol-rmcp` (rmcp bridge), `agent-client-protocol-tokio`, `agent-client-protocol-http`, and `agent-client-protocol-conductor`. The v1 wire stability contract holds since v1.0; see [`tech-stack.md`](tech-stack.md) for the pinned release.

The project should implement ACP rather than inventing a new IDE-agent protocol. Native plugins are still needed for capabilities that ACP does not expose or where the IDE does not provide a complete ACP client.

#### Provider-specific products

Claude Code, Codex, and Gemini increasingly include:

- worktrees;
- native IDE integration;
- sandbox modes;
- permissions;
- structured or headless output;
- agent SDKs or APIs.

A third-party product cannot win by offering older versions of those features. It must provide a provider-independent enforcement and context layer that remains useful even as individual harnesses improve.

---

## 26. Research sources

Accessed 2026-07-23 unless otherwise noted.

### Closest products and protocols

- Agent of Empires: https://github.com/agent-of-empires/agent-of-empires
- Agent of Empires sandbox documentation: https://github.com/agent-of-empires/agent-of-empires/blob/main/docs/guides/sandbox.md
- Conductor: https://www.conductor.build/
- Rivet Sandbox Agent: https://github.com/rivet-dev/sandbox-agent
- Agent Sandbox: https://github.com/mattolson/agent-sandbox
- Agent Workspace: https://github.com/agent-workspace/agent-workspace
- Codexia: https://github.com/milisp/codexia
- Agent Client Protocol: https://agentclientprotocol.com/
- ACP GitHub organization: https://github.com/agentclientprotocol
- JetBrains ACP announcement and documentation: https://blog.jetbrains.com/idea/2025/06/agent-client-protocol/
- Zed ACP documentation: https://zed.dev/docs/ai/acp

### Sandbox and workspace infrastructure

- Daytona: https://github.com/daytonaio/daytona
- Coder: https://github.com/coder/coder
- E2B: https://github.com/e2b-dev/E2B
- Firecracker: https://github.com/firecracker-microvm/firecracker
- Landlock documentation: https://landlock.io/
- Linux seccomp userspace API: https://www.kernel.org/doc/html/latest/userspace-api/seccomp_filter.html
- Rootless Podman: https://github.com/containers/podman

### Provider and harness references

- OpenAI Codex: https://github.com/openai/codex
- Anthropic Claude Code: https://github.com/anthropics/claude-code
- Gemini CLI: https://github.com/google-gemini/gemini-cli
- OpenCode: https://github.com/anomalyco/opencode
- Pi mono repository: https://github.com/badlogic/pi-mono
- Goose: https://github.com/block/goose

### Provider catalogs, Rust SDKs, and JetBrains integration

- models.dev repository and API documentation: https://github.com/anomalyco/models.dev
- models.dev provider catalog: https://models.dev/api.json
- models.dev provider-independent metadata: https://models.dev/models.json
- models.dev combined catalog: https://models.dev/catalog.json
- Official Rust MCP SDK (`rmcp`): https://github.com/modelcontextprotocol/rust-sdk
- `genai` multi-provider Rust library: https://github.com/jeremychone/rust-genai
- Rig Rust LLM framework: https://github.com/0xPlaygrounds/rig
- `async-openai`: https://github.com/64bit/async-openai
- `claude-api`: https://github.com/joshrotenberg/claude-api
- `gemini-rust`: https://crates.io/crates/gemini-rust
- JetBrains ACP documentation: https://www.jetbrains.com/help/ai-assistant/acp.html
- JetBrains agent activation and authentication: https://www.jetbrains.com/help/ai-assistant/activate-agents.html
- JetBrains MCP documentation: https://www.jetbrains.com/help/ai-assistant/mcp.html
- JetBrains third-party and OpenAI-compatible models: https://www.jetbrains.com/help/ai-assistant/use-custom-models.html
- JetBrains supported models and AI subscription: https://www.jetbrains.com/help/ai-assistant/supported-llms.html

### Project configuration standards

- AGENTS.md: https://agents.md/
- Agent Skills specification: https://agentskills.io/specification
- Model Context Protocol specification: https://modelcontextprotocol.io/specification/

### Arbitraitor

- Repository: https://github.com/arbsec/arbitraitor
- README and architecture: https://github.com/arbsec/arbitraitor/blob/main/README.md
- Plan-bound approval ADR: https://github.com/arbsec/arbitraitor/blob/main/docs/adr/0013-plan-bound-approval-capability.md
- Sandbox crate: https://github.com/arbsec/arbitraitor/tree/main/crates/arbitraitor-sandbox
- Policy engine: https://github.com/arbsec/arbitraitor/tree/main/crates/arbitraitor-policy
- MCP integration: https://github.com/arbsec/arbitraitor/tree/main/crates/arbitraitor-mcp

### Security context

- Git worktree documentation: https://git-scm.com/docs/git-worktree
- Visual Studio Code Workspace Trust: https://code.visualstudio.com/docs/editor/workspace-trust
- JetBrains project security guidance: https://www.jetbrains.com/help/idea/project-security.html
- OpenSSF malicious packages project: https://github.com/ossf/malicious-packages
- SLSA specification: https://slsa.dev/
- Sigstore Cosign: https://github.com/sigstore/cosign

---
