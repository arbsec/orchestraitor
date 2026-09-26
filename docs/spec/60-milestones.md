# Orchestraitor specification: Milestones and acceptance

## 998. MVP requirements

This section defines the minimum viable product. Each requirement carries measurable acceptance criteria and, where applicable, a performance budget. Subsystem details live in §9 (Major subsystems); this section consolidates the MVP-critical subset into a structured format and cross-references the full specification rather than duplicating it.

Performance budgets referenced here are enforced as CI gates in Appendix F. Developer experience budgets are consolidated in MVP-10.

### MVP-1: One complete golden path

The MVP must demonstrate this workflow end to end:

```text
orc init → detect project and existing agent tooling → import configuration non-destructively → launch native or wrapped harness → create managed workspace → expose built-in tools and approved MCP servers → apply transactional edits → format and run safe fixes → verify → review compact diff → promote or roll back → retain Arbitraitor receipts
```

**Acceptance criteria per step:**

| Step | Acceptance criteria | Cross-reference |
|---|---|---|
| `orc init` | Completes without a provider. Detects languages, formatters, package managers, Git layout, devcontainer, toolchain files, existing agent/MCP/IDE config, sensitive paths. Writes proposed `orchestraitor.toml` with `# Proposed by orc init` comments. `--dry-run` writes nothing. | §9.18, §9.20, §9.22.6 |
| Detect project and existing agent tooling | Detection is deterministic and local. Uncertain classification produces `general` domain, not a guess. Init summary reports what was detected and what remains uncertain. | §9.20, §9.21 |
| Import configuration non-destructively | Existing `CLAUDE.md`, `GEMINI.md`, `.mcp.json`, AGENTS.md, skills, hooks are imported without overwriting originals. `orc config import --dry-run` shows what would change. Backup, diff, rollback, and removal are supported. | §9.18, §9.18.2 |
| Launch native or wrapped harness | One native provider (Neuralwatt GLM-5.2) and one wrapped harness (Claude Code) launch inside an Arbitraitor-enforced environment. Adapter manifest is validated. | §10.1, §10.3, §10.6 |
| Create managed workspace | Snapshot workspace is created (no `.git` exposed to worker). Workspace mode, base commit, and trust state are recorded. | §9.4 |
| Expose built-in tools and approved MCP servers | Built-in filesystem, search, patch, process, Git, formatting, and verification tools are available without requiring generic filesystem or shell MCP servers. Approved MCP servers are fingerprinted and contained. | MVP-6, §9.18.1 |
| Apply transactional edits | Every mutation uses optimistic concurrency (read digest → apply patch → normalize → verify → produce final digest). Partial failure does not corrupt the workspace. | §9.5 |
| Format and run safe fixes | Project-configured formatter runs on write. Safe lint fixes apply automatically. Unsafe fixes require explicit policy or approval. Non-idempotent formatter is disabled and reported. | §9.5 |
| Verify | Project-configured verification commands run inside the sandbox. Results are recorded in the event store. Same registry works locally and in CI. | MVP-8, §9.5 |
| Review compact diff | TUI shows side-by-side and unified diffs. Normalization delta is bounded by token and byte budgets. Agent receives only information it did not already know. | §9.2, §9.5 |
| Promote or roll back | Promotion follows the output quarantine pipeline. Rollback is available within the overlay window. 100% rollback reliability for committed transactions. | §9.14, MVP-4 |
| Retain Arbitraitor receipts | Every security-sensitive operation produces an Arbitraitor receipt. Receipts are retained in the session event store. | §9.17, §16 |

**Performance budgets:**

| Metric | Budget | CI gate |
|---|---|---|
| `orc init` on warm cache | < 5 s | Appendix F |
| Daemon startup (warm) | < 100 ms p95 | Appendix F |
| Tool-call latency overhead | < 10 ms p95 | Appendix F |
| Format-on-write (files < 1000 lines) | < 200 ms | Appendix F |

### MVP-2: Adoption and shadow modes

The MVP must support incremental adoption through four commands, each with a clear enforcement story.

**`orc observe -- <harness>`**

Records compatibility, mutations, commands, network requests, and policy decisions without claiming enforcement. The output MUST clearly identify as non-protective. Use this to evaluate Orchestraitor against an existing workflow before committing to `orc wrap`.

Acceptance criteria:
- Observe mode records a normalized event stream for the target harness.
- The event stream includes: filesystem mutations, process executions, network requests, MCP tool calls, and policy decisions that would have been made.
- The TUI and `orc status` display a persistent "observation mode: non-protective" indicator.
- No enforcement is claimed or implied.

**`orc wrap -- <harness>`**

Launches an existing CLI harness inside an Arbitraitor-enforced environment. The harness runs inside the sandbox; its filesystem, network, process, and secret access are mediated by Arbitraitor.

Acceptance criteria:
- Wrapped harness cannot access host credentials, the main checkout, host `.git`, SSH keys, or cloud credentials (per §4.3 kill criteria).
- Wrapped harness events are normalized into the Orchestraitor event schema.
- Harness permission prompts are mapped into trusted control-plane approvals where technically possible.
- Unsupported harness-side privileges remain blocked by the outer sandbox.

**`orc connect <integration>`**

Configures an integration (harness, IDE, MCP server) with dry-run, backup, diff, rollback, and removal support.

Acceptance criteria:
- `orc connect <integration> --dry-run` shows what would change, writes nothing.
- `orc connect <integration>` applies with backup of replaced files.
- `orc connect <integration> --diff` shows current vs. proposed.
- `orc disconnect <integration>` restores from backup.
- `orc status` displays the active enforcement level per integration.

**`orc proxy`**

Runs `orcd` as a local OpenAI- and Anthropic-compatible provider facade. Existing harnesses route model traffic through Orchestraitor without immediately replacing their normal interface.

Acceptance criteria:
- OpenAI Responses API compatibility.
- OpenAI Chat Completions compatibility.
- Anthropic Messages API compatibility.
- `/v1/models` and capability discovery.
- Streaming and tool-call preservation.
- Short-lived local authentication tokens.
- Upstream BYOK routing without exposing the upstream credential to child processes.
- The proxy MUST NOT claim filesystem or shell containment when the external harness executes tools outside Arbitraitor. Stronger enforcement requires `orc wrap` or native mode.

**Policy shadowing:**

Reports what would have been allowed, denied, or approval-gated before enforcement. The shadow report is recorded in the event store and surfaced in the TUI. This lets users evaluate policy changes against recorded sessions without risking enforcement.

Acceptance criteria:
- Shadow policy decisions are recorded per operation with the decision outcome (`pass`, `pass_with_constraints`, `prompt`, `block`, `unsupported`, `defer_to_stronger_sandbox`).
- Shadow decisions do not affect execution.
- Shadow report is available via `orc policy check --shadow --session=<id>`.

**All setup operations require:**

- Dry-run: show what would change without writing.
- Diff: show current vs. proposed.
- Backup: preserve replaced files.
- Rollback: restore from backup.
- Removal: `orc disconnect` or `orc uninstall` removes Orchestraitor with no residue (under 30 seconds, per MVP-10).

Cross-reference: §9.18.2 (Migration and recovery UX), §10.1 (Integration modes), §10.8 (CLI, proxy, and migration experience).

### MVP-3: Explicit guarantee levels

Every session MUST display its effective guarantee level. No session may imply a stronger guarantee than the active platform backend and integration mode can enforce.

**Every session must show:**

| Field | Meaning | Source |
|---|---|---|
| Integration mode | `native` \| `wrapped` \| `mcp-gateway` \| `provider-proxy` \| `observe` | §10.1 |
| Workspace backend | `projected-vfs` \| `native-overlay` \| `materialized-workspace` | §9.4.2 |
| Filesystem enforcement | `Available` \| `Degraded` \| `Unavailable` | Arbitraitor capability probe |
| Process containment | `Available` \| `Degraded` \| `Unavailable` | Arbitraitor capability probe |
| Network containment | `Available` \| `Degraded` \| `Unavailable` | Arbitraitor capability probe |
| Secret protection | `brokered` \| `mounted-read-only` \| `unavailable` | §9.13 |
| MCP containment | `sandboxed` \| `unsandboxed` \| `none` | §9.18.1 |
| Host access | `none` \| `read-only` \| `full` | §9.4 workspace mode |
| Privileged-operation support | `polkit` \| `launchd-service` \| `windows-service` \| `none` | §9.32.4 |
| Known gaps | List of unsupported capabilities and fallbacks in use | §9.32.4 |

**Never imply provider-proxy mode secures tool execution performed by another harness.** The proxy provides provider routing, credential isolation, context optimization, telemetry, and auditability. It does not contain filesystem or shell actions performed independently by the external harness. The guarantee level display MUST state which actions remain outside the trust boundary.

Cross-reference: §9.32.4 (Per-session capability report), §10.1 Mode D (Provider-compatible proxy).

### MVP-4: Transactional workspace foundation

Every mutation is a transaction. The trusted checkout is never corrupted by partial failure, concurrent edits, or background processes.

**Transaction lifecycle:**

```text
capture base generation
  → stage requested changes
  → detect all side effects (secondary file changes, formatter output, fixer output, generator output)
  → normalize (format, safe fixes, convergence check)
  → verify (project-configured verification commands)
  → review (compact diff in TUI)
  → atomically promote or roll back
```

**Acceptance criteria:**

- **Crash recovery:** durable task state survives a daemon restart. `running` tasks transition to `orphaned`; `paused` tasks stay paused; `approval-required` and `input-required` tasks stay where they are. The user can resume from the latest checkpoint.
- **Optimistic concurrency:** every mutable file operation uses `read(path) → content + digest D1 → apply_patch(path, expected_digest = D1, patch) → normalization → final digest D2`. Next mutation must target D2. Stale digest fails with a clear conflict message.
- **Checkpoints:** long-running tasks emit periodic checkpoints (after every N tool calls or a configurable time budget). Checkpoints enable replay-from-checkpoint without re-running prior tool calls.
- **Conflicting IDE edits:** the controller detects base-branch drift and external mutations per generation. Worker wins within its workspace overlay; external mutations never win silently. Conflicts surface in the diff review view.
- **Background processes:** remain attached to a session generation and emit later mutation events. The controller reconciles by content digest after a bounded quiescence window.
- **Partial failure:** a task that fails or is cancelled preserves partial results (partial patches, completed tool calls, model responses) in the session's event store. The user can promote partial patches via the output quarantine.
- **No corruption of trusted checkout:** the original checkout and shared Git metadata are never modified by worker operations. Promotion is the only path from worker output to trusted state.

Cross-reference: §9.4 (Workspace and Git controller), §9.4.1 (edge cases), §9.5 (Filesystem transaction engine), §9.14 (Output quarantine and promotion), §9.24 (Task and session lifecycle).

### MVP-5: Project bootstrap and environment detection

`orc init` must locally detect the project environment without executing untrusted code and without an LLM or configured provider.

**Must detect:**

| Category | Examples |
|---|---|
| Languages and frameworks | Rust (`Cargo.toml`), TypeScript/JavaScript (`package.json`), Python (`pyproject.toml`), Go (`go.mod`), Java (`pom.xml`), and others |
| Package and build systems | npm, pnpm, yarn, bun, cargo, uv, pip, poetry, go modules, maven, gradle, nuget |
| Formatters, linters, tests, type checks | prettier, biome, eslint, rustfmt, gofmt, ruff, black, clang-format, ktfmt, dotnet format, dart format, zig fmt |
| Git layout | monorepos, nested repositories, submodules, sparse checkouts, Git LFS |
| Dev Container configuration | `devcontainer.json`, `Dockerfile`, `docker-compose.yml` |
| Toolchain files | Nix flakes, mise, asdf, `.tool-versions` |
| Existing agent/MCP/skills/IDE configuration | `AGENTS.md`, `CLAUDE.md`, `GEMINI.md`, Copilot instructions, Cursor rules, `.mcp.json`, `.vscode/mcp.json`, Agent Skills directories, agent hooks |
| Sensitive paths | `**/secrets/**`, `**/.aws/**`, `**/env.local`, credential-shaped files |
| Likely generated files | `node_modules/`, `target/`, `dist/`, `build/`, `.next/` |

**Import existing Dev Container configuration where useful.** The devcontainer may specify workspace paths, environment variables, lifecycle scripts, and extensions. Orchestraitor imports these as configuration proposals, not as trusted execution environments. Dev Container lifecycle scripts are treated as untrusted commands subject to Arbitraitor analysis (§9.10).

**Must work without an LLM or configured provider:**

- Initialization completes without failing.
- The `general` domain is always enabled.
- Uncertain classification produces `general` for that area, not a guess.
- The init summary reports what was detected and what remains uncertain; the user confirms or amends.
- Provider setup is offered as an optional next step.
- The harness MUST NOT require or silently request an API key.
- LLM-assisted detection MAY be offered later as an explicit opt-in enhancement. It is never required.

Cross-reference: §9.18 (Project initialization), §9.20 (Init without a provider), §9.21 (Domain detection heuristics).

### MVP-6: Built-in coding tools

The MVP ships structured coding tools without requiring generic filesystem or shell MCP servers. Prefer structured operations over Bash. Raw shell remains an explicit capability, not the primary tool surface. All enforcement belongs to Arbitraitor.

**Built-in tool surface:**

```text
fs.read
fs.stat
fs.list
fs.search
fs.apply_patch
fs.create
fs.rename
fs.remove
format.run
lint.run
check.run
test.run
task.run
```

**Acceptance criteria:**

- Filesystem tools use optimistic concurrency (digest-based) for mutable operations.
- `fs.apply_patch` returns a compact normalization delta, not a full file reread.
- `format.run` uses the project-configured formatter, detected at `orc init`.
- `lint.run` applies only safe fixes by default. Unsafe fixes require explicit policy or approval.
- `check.run` and `test.run` execute inside the Arbitraitor sandbox. Output is capped and summarized.
- `task.run` runs a curated task adapter (e.g., `cargo test`, `npm test`), not arbitrary shell.
- Raw shell is a capability with four modes: strict, standard, compatible, host. The outer sandbox remains authoritative even when a wrapped harness believes it has unrestricted shell access.
- No generic filesystem MCP server or shell MCP server is required for the MVP golden path.

**Shell policy:**

- **Strict:** Shell unavailable except curated task adapters.
- **Standard:** Shell sandboxed, statically planned, observed, and reconciled.
- **Compatible:** Broad shell access inside the outer sandbox.
- **Host:** Harness-native behavior with explicit loss-of-containment warning.

Cross-reference: §9.5 (Filesystem transaction and normalization engine, shell policy), §9.18 (MCP and tool drift).

### MVP-7: Session durability

Persist a versioned event stream sufficient to resume after daemon, terminal, or harness failure.

**The event stream must include:**

| Category | Contents | Cross-reference |
|---|---|---|
| Task state | State machine transitions (queued, running, paused, completed, failed, cancelled, rejected, orphaned) | §9.24 |
| Context receipts | Selected items, omitted count, budget, index digest, provenance | §9.15, §9.15.1 |
| Workspace generation | Base commit, generated patches, output promotions | §9.4, §9.14 |
| Outstanding approvals | Pending approval requests, plan digests, expiry | §9.9 |
| Running processes | Process tree, resource usage, cancellation state | §9.27 |
| Provider usage | Model, provider, tokens, cost, routing decision | §9.19.4 |
| Tool results | Tool name, call id, duration, status, arguments (opt-in), results (opt-in) | §9.17 |
| Verification evidence | Check, test, lint, format results | §9.5 |
| Arbitraitor receipts | Verdicts, approvals, effective-control reports, security findings, output-promotion records | §9.17, §16 |

**Must support:**

- **Cancellation:** bounded grace period, cancellation token propagated to workers, process and resource cleanup via Arbitraitor's resource-release path. Anything that could not be released cleanly is recorded.
- **Recovery:** durable task state survives daemon restart. `orphaned` tasks are detected within a configurable heartbeat interval (default 30 s). The user can resume from the latest checkpoint.
- **Cleanup:** workspace snapshots for cancelled sessions are retained for a configurable window for post-mortem, then cleaned up.
- **Privacy-preserving export:** `orc evidence export --session=<id>` produces a redacted archive (file contents, prompts, completions, tool arguments, MCP payloads, and secrets always redacted). Reproducible state-machine reconstruction keeps the audit trail even when payloads are redacted.
- **Tamper detection:** hash-chained event records. A gap or hash mismatch fails the export/import validator.
- **Schema versioning:** events carry `schema_version`. Unknown future versions are preserved, not silently dropped, and flagged as `uninterpreted` in the replay UI.

Cross-reference: §9.17 (Event and receipt store), §9.17.1 (Forensic reconstruction), §9.24 (Task and session lifecycle), §9.27.4 (Cancellation releases resources).

### MVP-8: Headless and CI support

All core workflows must operate interactively and non-interactively. Stable JSON output and documented exit codes are required for automation.

**Commands:**

```sh
orc verify                    # run project-configured verification
orc policy check              # evaluate policy against a plan or session
orc run --non-interactive      # run a task without TUI interaction
orc evidence export            # export session evidence (privacy-preserving)
```

**Acceptance criteria:**

- Every machine-oriented command supports `--json`, `--quiet`, `--non-interactive`, explicit project and config paths, stable schemas, and documented exit codes.
- `orc verify` runs the same detected verification registry that works locally and in CI. The registry maps recognized configuration files and lockfile-resolved tools to verification commands.
- `orc policy check` evaluates policy against a plan or recorded session and reports decisions in JSON.
- `orc run --non-interactive` executes a task without TUI interaction. Approvals follow the configured non-interactive policy (default: block).
- `orc evidence export` produces a privacy-preserving archive suitable for CI artifacts or bug reports.
- Exit codes are documented and stable. Non-zero exit indicates failure. Specific exit codes distinguish security blocks, verification failures, configuration errors, and infrastructure failures.

**Same detected verification registry works locally and in CI:**

The verification registry is part of the project configuration (`orchestraitor.toml`). It maps recognized configuration files to verification commands. The same registry runs in local interactive sessions and in CI non-interactive sessions. Results are recorded in the event store and surfaced via `orc verify --json`.

Cross-reference: §9.5 (verification commands), §9.18 (CLI commands), §10.8 (CLI, proxy, and migration experience).

### MVP-9: Compatibility and conformance

Maintain fixtures and automated compatibility tests for every supported combination. Report supported, degraded, experimental, or broken status.

**Must maintain fixtures and conformance tests for:**

| Category | Examples |
|---|---|
| Provider protocols | OpenAI Responses, OpenAI Chat Completions, Anthropic Messages |
| MCP versions | `rmcp` baseline |
| ACP versions | `agent-client-protocol` baseline |
| Wrapped harnesses | Claude Code, Codex CLI, Gemini CLI, OpenCode, Pi |
| IDE integrations | JetBrains, VS Code, Zed |
| Workspace backends | `projected-vfs`, `native-overlay`, `materialized-workspace` |
| Model providers | Neuralwatt, Z.ai, Anthropic, Google, OpenAI-compatible |
| Code-intelligence MCP servers | LSP, tree-sitter, content-addressed index |

**Combination classification:**

- **Supported:** cassette + event trace + integration test pass.
- **Degraded:** subset works; specific features flagged unavailable.
- **Experimental:** passes locally; not gated in CI.
- **Broken:** known to fail; either fix in flight or marked unsupported in `doctor`.

**Acceptance criteria:**

- Conformance is verified behaviorally during upgrades, not just by reading version strings.
- Adapter behavior is checked against the recorded cassette. Breaks during upgrade surface as `broken` rather than silent wrong behavior.
- Protocol fields the adapter does not interpret are preserved under `unknown_protocol_fields` rather than silently dropped.
- `orc doctor` reports the combination matrix.
- The release-notes generator includes conformance changes.

Cross-reference: §9.30 (Compatibility and conformance suite), §21.7 (Compatibility and conformance suite testing).

### MVP-10: Developer experience budgets

These budgets are enforced as CI gates (Appendix F) and surfaced in `orc doctor`. Failure to meet a budget blocks release or requires an explicitly documented override.

| Metric | Target | CI gate |
|---|---|---|
| Installation steps | < 5 commands | Documentation check |
| `orc init` duration (warm cache) | < 5 s | Appendix F |
| Daemon startup (warm) | < 100 ms p95 | Appendix F |
| Idle memory (daemon) | < 60 MB RSS | Appendix F |
| Idle memory (TUI) | < 35 MB RSS | Appendix F |
| Tool-call latency overhead | < 10 ms p95 | Appendix F |
| Filesystem overhead | < 5% vs raw | Appendix F |
| Context-token savings | 30% median reduction | Appendix F |
| Rollback reliability | 100% for committed transactions | Integration test |
| Time to disable/remove Orchestraitor | < 30 s, no residue | Integration test |

**Installation steps (< 5 commands):** A new user can install Orchestraitor, initialize a project, and start a session in fewer than 5 commands. Example: `cargo install orchestraitor`, `orc init`, `orc run claude`.

**Context-token savings (30% median reduction):** Measured against direct harness baseline on a representative repository task suite. Less than 3% relative task-success regression. No hidden truncation of security-relevant findings. See §13.5 for the full token efficiency budget.

**Time to disable/remove (< 30 s, no residue):** `orc disconnect <integration>` restores previous configuration. `orc uninstall` removes the daemon, config, and state. No orphaned processes, modified files, or residual configuration. The user's existing tooling works exactly as it did before installation.

Cross-reference: §13 (Performance and footprint requirements), §13.1 (Baseline budgets), §13.5 (Token efficiency budgets), Appendix F (Performance CI gates).

---

## 999. High-value differentiators after MVP

This section defines post-MVP features that build on the MVP foundation. They are explicitly separated from MVP requirements. Each item assumes the MVP trust model, transactional workspace, and Arbitraitor integration are already working. None of these features may bypass Arbitraitor invariants (§2.2) or implement security logic independently inside Orchestraitor (§16).

### 1. Policy and action simulator

Preview planned actions before execution: capabilities, affected files and services, network destinations, secrets, cost, verification plan, approval boundaries, and rollback path. Simulate policy changes against recorded sessions to see what would have been allowed, denied, or approval-gated.

This extends MVP-2's policy shadowing from "what would have happened" to "what would happen if I change this policy." The simulator runs against recorded event streams (§9.17) and produces a diff of decisions without executing anything.

### 2. Semantic change ledger

Beyond a git diff: associate each change with task requirement, agent action, formatter, fixer, generator, MCP server, privileged operation, verification evidence, and user approval. Provide a "why did this change?" view and a review proof bundle.

The ledger builds on the transaction engine (§9.5) and the change attribution system (agent-authored, formatter-authored, safe-fixer-authored, generator-authored, unexpected side effect, user-authored). Each entry links the change to its causal chain, the approval that authorized it, and the verification that confirmed it.

### 3. Explainable context compiler

Expose why each context item was selected: trust class, provenance, token cost, omitted items, cache hits, expected relevance, and what changed since the previous context build. Provide pin, exclude, replace, and inspect rules for individual context items.

This extends the context compiler (§9.15) and its provenance envelope (§9.15.1) from "what was selected" to "why it was selected and what you can do about it." The user can override the compiler's choices and see the effect on token cost and task success.

### 4. Project feedback distillation

Learn from rejected changes, review comments, user corrections, repeatedly failing checks, post-agent manual edits, and model/domain performance. Propose updates to instructions, skills, routing, and verification. Never modify durable project knowledge automatically.

This is an advisory system, not an autonomous one. It surfaces patterns (e.g., "the frontend agent's patches are rejected 40% of the time; the common cause is missing CSS import in the test fixture") and proposes changes. The user approves every modification to project configuration.

### 5. Earned autonomy

Track evidence per task class and repository. Recommend increased or reduced autonomy based on verification pass rates, unexpected mutations, review acceptance, rollback frequency, policy violations, model reliability, and task risk.

Autonomy levels range from "prompt for every action" to "auto-approve repeated identical actions within a session" to "auto-approve within a task class." Users and org policy retain final control. The system never silently increases autonomy; it recommends, the user decides.

### 6. Model shadowing and controlled experiments

Run candidate models and prompts in shadow without applying changes. Compare plan quality, patch correctness, verification success, review acceptance, latency, token use, and cost. Support canary promotion and rollback.

This extends the shadow evaluation in §9.31.3 from "does the new model work?" to "is the new model better, and by how much?" The comparison is recorded in the cost ledger (§9.19.4) and the event store (§9.17) for reproducible analysis.

### 7. Time-travel and session branching

Branch from any checkpoint: same task different model, same patch different reviewer, same plan stricter policy, same context different prompt. Compare diffs, evidence, cost, and verification results across branches.

This builds on the checkpoint system in §9.24.2 and the session durability in MVP-7. Each branch is an independent session with its own workspace, event stream, and receipts. Branches never corrupt the original session.

### 8. Knowledge-index federation

Provide a common interface over built-in indexing, LSP, Serena, CodeGraph, and codebase-memory. Share project identity, file generations, and invalidation events across indexes. Each index remains isolated and attributable.

This extends the context compiler (§9.15) and the LSP integration (§9.16) from "one index" to "federated indexes query." The federation layer routes queries to the appropriate index, merges results, and attributes each result to its source. No index gains authority over another.

### 9. System Assistance mode

Privileged diagnostic and staged repair workflow. Synthetic shadow system root where available. Opt-in, separately capability-reported.

System Assistance mode is for diagnosing and repairing system-level issues (broken package manager, corrupted Git state, misconfigured toolchain) that require elevated privileges. It uses a synthetic shadow system root where the platform supports one, so repairs are tested before promotion to the real system. The mode is opt-in, capability-reported separately, and never active by default.

### 10. Signed team packs

Distribute signed bundles: org policy, agent profiles, model routing, skills, MCP definitions, verification rules, formatting policy, and approved environments. Separate mandatory policy ceilings from overridable defaults.

This extends the signed team policies in §9.22.8b from "signed policy file" to "signed bundle of everything a team needs." The pack is signed with minisign or cosign (per Arbitraitor `arbitraitor-receipt` signing API). Mandatory policy ceilings cannot be overridden by lower layers; overridable defaults can be tightened but not weakened without the explicit, audited override path (§9.22.9).

---

## 22. Delivery milestones

The delivery structure uses five milestones. Each milestone has explicit exit criteria covering security guarantees, compatibility limits, migration path, rollback behavior, and user-facing diagnostics. Do not call a milestone complete until its security guarantees, compatibility limits, migration path, rollback behavior, and user-facing diagnostics are tested.

### M0: Arbitraitor prerequisites and architecture validation

**Scope:**

- Verify Arbitraitor provides the effective-control probes, approval token issuer, plan-context binding, receipt schema, and sandbox backends the MVP requires (see §16.2 mandatory workflow).
- Validate the `arbitraitor_sandbox::compute_effective_controls()` probe matrix on Linux (Landlock, seccomp, namespaces, cgroups).
- Validate `arbitraitor_mcp::ApprovalTokenIssuer` wiring with explicit `McpServer` construction (not the default stdio server; see §9.9).
- Validate `arbitraitor_exec::ExecutionContextBuilder::from_operation(...)` receipt matrix.
- Confirm Arbitraitor remains independently buildable and does not depend on Orchestraitor types (§16.3).
- Prototype the `materialized-workspace` backend (snapshot mode, `gix`-based, no VFS mediation) on Linux.
- Prototype the daemon, TUI, and CLI skeleton with config resolution (§9.22).

**Exit criteria:**

- **Security guarantees:** Arbitraitor capability probes report `Available` for filesystem isolation, process tree containment, and privilege suppression on the reference Linux platform. Missing controls fail closed (§6.7).
- **Compatibility limits:** Linux only. macOS is not claimed. `materialized-workspace` backend only; `projected-vfs` and `native-overlay` are not yet available.
- **Migration path:** No migration needed (greenfield). `orc init` writes a proposed `orchestraitor.toml` with `# Proposed by orc init` comments (§9.22.6).
- **Rollback behavior:** `orc init --dry-run` shows what would be written. No existing tooling is modified.
- **User-facing diagnostics:** `orc doctor` reports Arbitraitor version, capability probe results, and any missing prerequisites.

### M1: Linux golden path with one native provider and one wrapped harness

**Scope:**

- Linux golden path end to end (see MVP-1 in §998):
  `orc init` → detect project and existing agent tooling → import configuration non-destructively → launch native or wrapped harness → create managed workspace → expose built-in tools and approved MCP servers → apply transactional edits → format and run safe fixes → verify → review compact diff → promote or roll back → retain Arbitraitor receipts.
- One native provider: Neuralwatt with GLM-5.2 (see §10.3, Appendix B).
- One wrapped harness: Claude Code (see §10.1 Mode C, Appendix D).
- Domain-agent catalog (8 domains, 5 roles; see §9.19).
- Snapshot workspace (no `.git` exposed to worker; see §9.4).
- Native sandbox via Arbitraitor (Linux: Landlock + seccomp + namespaces; see §9.6, §9.32).
- MCP server + MCP client/loader (see §9.18, §10.5).
- Arbitraitor integration via path or pinned-git dependency (see [`tech-stack.md`](tech-stack.md) §3); approval and execution capabilities wired explicitly (see §9.9).
- Normalized events, basic policy, diff review.
- Context compiler prototype.
- Performance harness.
- Cost and subscription ledger (see §9.19.4–§9.19.6).
- Domain detection at `orc init` (see §9.20, §9.21).
- Spec-driven autonomous delivery (secondary goal; see §9.33): backlog decomposition, task DAG, isolated implementation w/ fresh contexts, adversarial review loop, durable orchestration state, `orc backlog` CLI controls.
- Adoption and shadow modes: `orc observe`, `orc wrap`, `orc connect`, `orc proxy` (see MVP-2 in §998).
- Explicit guarantee levels surfaced in every session (see MVP-3 in §998).
- Transactional workspace foundation (see MVP-4 in §998).
- Built-in coding tools without requiring generic filesystem or shell MCP servers (see MVP-6 in §998).
- Session durability with versioned event stream (see MVP-7 in §998).
- Headless and CI support: `orc verify`, `orc policy check`, `orc run --non-interactive`, `orc evidence export` (see MVP-8 in §998).

**Exit criteria:**

- **Security guarantees:** The kill criteria in §4.3 are met. A wrapped harness cannot reach host credentials, the main checkout, host `.git`, SSH keys, or cloud credentials. A malicious repository configuration attack that succeeds in a conventional worktree setup is blocked by output promotion. Arbitraitor receipts are retained for every security-sensitive operation.
- **Compatibility limits:** Linux only. One native provider (Neuralwatt GLM-5.2). One wrapped harness (Claude Code). `materialized-workspace` backend only. macOS, WSL2, and Windows native are not claimed.
- **Migration path:** `orc init` detects and proposes configuration non-destructively. `orc connect --dry-run` previews changes. `orc disconnect` restores previous configuration. All setup operations support dry-run, diff, backup, rollback, and removal (§9.18.2).
- **Rollback behavior:** 100% rollback reliability for committed transactions. Partial failure does not corrupt the trusted checkout. Crash recovery transitions `running` tasks to `orphaned` and preserves partial results (§9.24).
- **User-facing diagnostics:** `orc status` displays integration mode, workspace backend, filesystem enforcement, process containment, network containment, secret protection, MCP containment, host access, privileged-operation support, and known gaps (MVP-3). `orc doctor` tests streaming, tool calls, models, credentials, sandbox controls, and unsupported capabilities.

### M2: macOS parity and additional provider/harness adapters

**Scope:**

- macOS parity using `materialized-workspace` backend (same `gix` snapshot as Linux; see §9.32.3.2).
- Arbitraitor capability probe reports macOS containment state (`seatbelt`/`sandbox-exec` where sufficient, `Degraded` or `Unavailable` where not).
- Additional provider adapters: Anthropic Messages API, Google Gemini native API, Z.ai GLM-5.2 (see §10.3).
- Additional wrapped harness adapters: Codex CLI, Gemini CLI, OpenCode, Pi (see §10.1 Mode C).
- Provider-compatible proxy (Mode D): OpenAI Responses, OpenAI Chat Completions, Anthropic Messages compatibility (see §10.1).
- models.dev integration with live fetch + caching + bundled fallback (see §10.4).
- Provider capability verification matrix (see §9.29).
- Compatibility and conformance suite with recorded fixtures (see §9.30, MVP-9 in §998).
- Headless CLI with stable JSON output and documented exit codes (see MVP-8 in §998).

**Exit criteria:**

- **Security guarantees:** macOS capability report is honest about the difference from Linux. Strict mode fails closed when `seatbelt`/`sandbox-exec` is unavailable. Standard mode offers explicit degraded capability report where policy permits. Never advertise a stronger guarantee than the active platform backend can enforce (§9.32.6).
- **Compatibility limits:** Linux + macOS. Multiple providers and wrapped harnesses. `materialized-workspace` backend only on both platforms. WSL2 and Windows native are not claimed. `projected-vfs` and `native-overlay` backends are not yet available.
- **Migration path:** Existing M1 configurations migrate forward. `orc config migrate` applies forward-only migrations with backup (§9.22.8). Schema versions are stamped.
- **Rollback behavior:** Same as M1. `orc config migrate --undo` reverts the most recent migration.
- **User-facing diagnostics:** `orc doctor` reports per-platform capability matrix. Conformance suite results are surfaced via `orc doctor` and the release-notes generator. Combinations are classified `supported` / `degraded` / `experimental` / `broken` (§9.30.2).

### M3: IDE integrations, proxy/gateway maturity and workplace pilot

**Scope:**

- JetBrains plugin (IntelliJ IDEA, WebStorm, PyCharm, GoLand, RustRover, CLion, Rider; see §11.2).
- VS Code extension (see §11.3).
- Zed ACP integration (see §11.4).
- Neovim plugin and terminal editor support (see §11.5).
- ACP server/client support (see §11.1).
- Provider-compatible proxy maturity: streaming, tool-call preservation, short-lived local auth tokens, upstream BYOK routing (see §10.1 Mode D).
- MCP tool gateway maturity: typed filesystem, search, patch, process, Git, formatting, and verification tools exposed via MCP (see MVP-6 in §998).
- Workplace pilot with signed team policies (see §9.22.8b).
- `projected-vfs` and `native-overlay` backend prototypes on Linux (see §9.4.2).
- WSL2 support: Linux guest enforcement applies; Windows host actions clearly out of scope (see §9.32.3.3).

**Exit criteria:**

- **Security guarantees:** IDE plugins open session workspaces in untrusted or restricted mode. Agent-generated IDE configuration remains disabled until promoted (§9.14, §15.6). The trusted IDE plugin does not auto-trust agent-generated project configuration. Provider-proxy mode never claims filesystem or shell containment when the external harness executes tools outside Arbitraitor (§10.1 Mode D).
- **Compatibility limits:** Linux + macOS + WSL2 (Linux guest). IDE integrations for JetBrains, VS Code, Zed, Neovim. `projected-vfs` and `native-overlay` backends are experimental on Linux, not yet default. Windows native is not claimed.
- **Migration path:** `orc connect jetbrains`, `orc connect vscode` configure IDE integrations with dry-run, backup, and rollback support (§9.18.2). Existing M2 configurations migrate forward.
- **Rollback behavior:** `orc disconnect <integration>` restores previous configuration for every IDE integration. IDE plugin removal leaves no residue.
- **User-facing diagnostics:** `orc status` displays per-integration enforcement level (e.g., `claude-code: managed-process`, `my-mcp-server: mcp-tool-gateway`, `local-ollama: provider-proxy`). `orc doctor <integration>` tests streaming, tool calls, models, credentials, sandbox controls, and unsupported capabilities per integration.

### M4: advanced context, learning, experimentation and system assistance

**Scope:**

- Advanced context compiler features: semantic change ledger, explainable context compiler, project feedback distillation (see §999, High-value differentiators after MVP).
- Earned autonomy: track evidence per task class and repository; recommend increased or reduced autonomy (see §6).
- Model shadowing and controlled experiments: run candidate models/prompts in shadow without applying changes (see §6).
- Time-travel and session branching: branch from any checkpoint (see §6).
- Knowledge-index federation: common interface over built-in indexing, LSP, Serena, CodeGraph, codebase-memory (see §6).
- System Assistance mode: privileged diagnostic and staged repair workflow, opt-in (see §6).
- Signed team packs: distribute signed bundles of org policy, agent profiles, model routing, skills, MCP definitions, verification rules, formatting policy, approved environments (see §6).
- `projected-vfs` and `native-overlay` backends promoted from experimental to default on Linux where conformance passes (see §9.4.2).
- macOS `projected-vfs` / `native-overlay` prototypes via FSKit and APFS copy-on-write clones (see §9.32.3.2).
- Windows native backend prototype (separate security backend, not a thin WSL wrapper; see §9.32.3.4).
- Remote worker support with mutually authenticated TLS (see §8.4).
- Enterprise policy distribution and org policy layering (see §9.8, §9.22.8b).

**Exit criteria:**

- **Security guarantees:** Advanced features (earned autonomy, model shadowing, system assistance) never bypass Arbitraitor invariants. System Assistance mode is opt-in, separately capability-reported, and uses a synthetic shadow system root where available. Signed team packs separate mandatory policy ceilings from overridable defaults. Users and org policy retain final control over autonomy decisions.
- **Compatibility limits:** Linux + macOS + WSL2 + Windows native (prototype). `projected-vfs` and `native-overlay` backends are default on Linux where conformance passes; experimental on macOS. Remote workers are supported with a separate threat model.
- **Migration path:** Existing M3 configurations migrate forward. Signed team packs are versioned and migratable. `orc config migrate` handles schema evolution across versions.
- **Rollback behavior:** Time-travel and session branching preserve the original session. Model shadowing never applies changes. Earned autonomy recommendations are proposals, not automatic changes to durable project knowledge. System Assistance mode stages repairs rather than applying them directly.
- **User-facing diagnostics:** `orc doctor` reports advanced feature availability, capability reports for System Assistance mode, signed team pack verification status, and knowledge-index federation health.

---

### 22.6 Supply-chain and release requirements

Orchestraitor sits between users, source code, credentials, and agents. Its own update chain is part of the security model. From the first release:

- signed release artifacts (minisign or cosign per Arbitraitor `arbitraitor-receipt` signing API);
- checksums (SHA-256, published alongside the release);
- SBOM (CycloneDX via `cargo-cyclonedx` + release metadata);
- provenance attestations (GitHub artifact attestations);
- dependency + license policy (`cargo-deny` + `cargo-audit` in CI — see §21.10);
- pinned Arbitraitor commit (git rev `099d2c6` per tech-stack §2.1; updated to semver tag when available);
- protected release workflow (GitHub Actions with SHA-pinned actions; no `if: github.actor == '...'` trust patterns);
- security disclosure policy (private vulnerability reporting via GitHub Security Advisories; per `SECURITY.md`);
- update rollback (`orc update rollback` reverts to previous version + config migration undo per §9.22.8b);
- restricted use of install scripts (no `curl | sh` pattern; downloads go through Arbitraitor inspection per §16 dependency direction);
- explicit unsafe-code policy (§21.2.7 Miri: `forbid(unsafe_code)` in core Orchestraitor crates; unavoidable OS-specific unsafe code belongs in isolated Arbitraitor crates with safety comments + 2 maintainer approvals per Arbitraitor `conventions.md`).

### 22.7 Installation, upgrade, and removal tests

For a security product, installation is part of the trust boundary. The test suite (§21) MUST cover:

- fresh install;
- upgrade from previous version;
- downgrade where supported;
- uninstall (complete removal);
- config migration across versions (§9.22.8b);
- daemon / service registration + unregister;
- shell completions (`orc completions bash/zsh/fish/nu`);
- stale process cleanup (orphaned `orcd` processes, stale sockets, stale workspaces);
- removal without deleting user projects (workspace snapshots + event stores in `~/.local/share/orchestraitor/` are NOT removed by `orc uninstall`; user must `orc data delete --all` explicitly);
- reinstall after partial failure (interrupted install recovers);
- release signature + checksum verification (`orc update verify <manifest>` per Arbitraitor's `verify-update-manifest` pattern).

Define exactly what remains after uninstall: receipts, user-approved state, and any `~/.config/orchestraitor/` files. State what is removed vs. what persists.

### 22.8 Subscription OAuth guidance

For nice-to-have integrations with subscription-based harnesses (Codex/ChatGPT login, Gemini CLI/Google login, Claude Code/Claude Console authentication), **wrapping the official CLI should be the default approach before trying to reuse its OAuth credentials directly.**

Those are supported authentication flows for their respective clients. Their documentation does not automatically imply that third-party harnesses may safely or contractually reuse the resulting credentials. Direct OAuth credential reuse MUST be treated as separate feasibility work behind an explicit security review (§21.1) — never as a default integration path. Wrapping the official clients preserves their supported login flow while Orchestraitor supplies containment, supervision, and workspace control.

---

## 23. MVP acceptance criteria

This section defines what "MVP supported" means. Anything outside the matrix below MAY exist experimentally without blocking the MVP.

### 23.1 Precise supported matrix

```text
Platforms:
  Linux:   fully supported (reference platform)
  macOS:   fully supported (materialized-workspace backend; seatbelt where available)

Interfaces:
  CLI
  TUI

Providers:
  OpenAI-compatible endpoint (Neuralwatt GLM-5.2 is the primary)
  deterministic mock provider (§21.3 simulator)

Agent execution:
  one native Orchestraitor agent (direct provider mode, §10.1 Mode A)
  spec-driven autonomous delivery (§9.33)

MCP:
  project-scoped MCP gateway (§17.2)
  one local stdio server
  one remote HTTP server

Workspace:
  materialized-workspace (snapshot) backend — strong on Linux, portable on macOS

Arbitraitor integration:
  git-rev pinned (099d2c6) + local path override for dev
```

Anything not listed above (e.g., Anthropic-compatible provider, wrapped CLI harnesses, GUI, ACP/IDE plugins, `projected-vfs` / `native-overlay` backends, WSL2/Windows) is explicitly out-of-MVP-scope but architecture MUST not preclude it.

### 23.2 Pilot acceptance criteria

The workplace pilot deployment MUST have objective exit criteria:

```text
✓ setup completed in under N minutes (configurable target: 5 min)
✓ existing project imported without destructive changes (orc init w/ --dry-run)
✓ one real task completed and rolled back successfully (§9.4.3 history graph)
✓ one real task promoted successfully (§9.14 output promotion)
✓ worker crash recovered without losing state (§9.24 orphaned → checkpoint resume)
✓ dangerous mock tool call denied (§21.4 adversarial E2E)
✓ project-specific MCP isolation verified (§17.2.3 — project A cannot see project B)
✓ logs sufficient to diagnose a forced failure (§9.17 event store + §9.34 error taxonomy)
✓ no secrets appear in logs or receipts (§9.23.4 redacting layer)
✓ performance overhead remains below defined budgets (§21.9 performance benchmarks)
```

Without this, "MVP works" can become subjective. The pilot MUST be run against the deterministic simulator (§21.3) first — live-provider smoke is optional.

### 23.3 MVP checklist (ten outcomes)

The MVP is frozen around these ten outcomes. Everything outside is explicitly post-MVP:

1. Secure transactional workspace with history (§9.4.3), rollback (§9.4.3, §9.14), and promotion (§9.14).
2. Arbitraitor-enforced filesystem (§9.4.2), process (§9.6), and network (§9.12) controls.
3. Global and project provider/model configuration (§9.19.2, §9.22, §9.23).
4. Global and project agent templates plus custom agents (§9.19.1, §9.22.4).
5. Durable daemon (§17.2, §9.24) with isolated agent (§17.2.1) and MCP (§17.2) processes.
6. Project-scoped MCP gateway (§17.2 — one registration, project-specific resolution, isolation proven).
7. High-quality CLI and TUI with structured state (§9.2, §9.34) and errors (§9.34).
8. Deterministic provider (§21.3) and hostile MCP simulation (§21.4).
9. Full unit, integration, contract, and local adversarial E2E suites (§21.1-§21.4).
10. Observability (§20.4), performance budgets (§13, §21.9), and a truthful capability report (§9.32.4).

> Agents can work productively, fail safely, and leave enough evidence to understand exactly what happened.

### 23.4 Abrupt-termination E2E tests

The E2E suite MUST test abrupt termination at every significant phase:

```text
kill during workspace creation
kill during model streaming
kill during file write (fs.apply_patch mid-transaction)
kill during formatting (format.run mid-pass)
kill during verification (test.run/check.run)
kill during promotion (§9.14 output promotion mid-copy)
kill during rollback (§9.4.3 history graph mid-restore)
```

**Core invariant**: none of these may corrupt the trusted checkout (§6.2 — a worktree is not a sandbox; the trusted controller owns Git metadata; §9.5 optimistic-concurrency digest guarantees atomicity).

---

## 24. Open questions

1. Should the worker receive any Git binary, or only a read-only synthetic Git view?
2. Can CLI subscription authentication be brokered without violating provider terms or breaking flows?
3. Which closed CLIs expose sufficiently stable structured protocols?
4. Should the project reuse Rivet Sandbox Agent adapters or define a separate adapter protocol?
5. Can output promotion integrate cleanly with IDE project-trust mechanisms?
6. Which GUI toolkit meets accessibility, diff, terminal, and footprint requirements?
7. How much semantic indexing can run continuously without violating footprint goals?
8. Is SQLite sufficient for the event and index workload?
9. Should the network broker terminate TLS, or prefer destination-scoped CONNECT mediation?
10. Which operations require full content inspection before promotion?
11. How are signed policy and plugin updates distributed?
12. How should Windows containment be represented before a strong backend exists?
13. What provider telemetry is reliable enough for token accounting?
14. How should context selection quality be benchmarked independently of model variance?
15. Should repository policies be allowed to request capabilities, or only tighten them?

---

## 24. Recommended immediate implementation decisions

0. **Use the product name Orchestraitor, repository `arbsec/orchestraitor`, canonical CLI `orc`, long-form alias `orchestraitor`, and daemon `orcd`.**
1. **Use Rust for the daemon, TUI, policy, workspace control, brokers, and adapter host.**
2. **Treat Arbitraitor (`arbsec/arbitraitor`) as the sole security implementation and authority; add every missing security feature there before integrating it into Orchestraitor.**
3. **Treat Linux as the first reference security platform.**
4. **Use snapshot workspaces without `.git` as the strict/default prototype.**
5. **Implement ACP before inventing another IDE-agent protocol.**
6. **Build JetBrains and VS Code plugins as thin daemon clients.**
7. **Start with CLI wrappers, but design the adapter interface for direct providers from day one.**
8. **Make the context compiler independently benchmarkable and disableable.**
9. **Make output promotion mandatory for trust-sensitive file classes.**
10. **Publish performance budgets early and test them in CI.**
11. **Do not mount host credentials into workers in the strict design.**
12. **Do not permit plugins inside the trusted daemon unless first-party and audited.**
13. **Do not market generic worktrees or optional Docker as the security feature.**
14. **Use existing projects as integration targets where practical rather than rebuilding commodity layers.**
15. **Make project-aware format-on-write opt-out at initialization, with compact normalization deltas returned to agents.**
16. **Expose typed filesystem tools as the native mutation path and treat raw shell as a mediated capability.**
17. **Use `rmcp` for MCP and ACP for IDE interoperability rather than inventing competing protocols.**
18. **Use models.dev as an advisory cached catalog, verified against endpoint discovery and explicit user configuration.**
19. **Prototype provider transport with `genai`, but keep provider traits and native feature extensions project-owned.**
20. **Treat JetBrains AI subscription integration as an MCP/IDE mode unless JetBrains publishes a supported generic provider API.**
21. **Expose `orcd` through native harness, managed wrapper, MCP tool gateway, machine-friendly CLI, and OpenAI/Anthropic-compatible proxy surfaces.**
22. **Never claim filesystem or shell containment in proxy-only mode when the external harness executes tools outside Arbitraitor.**
23. **Make `orc connect`, `orc wrap`, `orc env`, and `orc doctor` reversible, inspectable, and explicit about enforcement level.**

---

## 25. Final assessment

The idea has already been partially built several times, but not as one coherent system.

The crowded portions are:

- multi-agent dashboards;
- worktree management;
- terminal session persistence;
- containerized coding agents;
- desktop wrappers around Claude and Codex;
- remote sandboxes;
- IDE-to-agent protocols.

The less occupied portion is:

- one low-footprint trusted control plane;
- enforced across both native providers and wrapped harnesses;
- with controller-owned Git;
- static plan-bound capabilities;
- brokered secrets and network;
- output quarantine and promotion;
- provider-independent context optimization;
- native TUI, GUI, and IDE clients;
- signed, explainable receipts.

That is a legitimate product direction.

It is also substantially harder than writing an agent harness. The difficult work is not the model loop. It is maintaining a correct security boundary across operating systems, harness versions, IDEs, build tools, package managers, and user overrides while remaining fast enough that developers leave it running all day.

The project is worth pursuing only if security and token savings remain the architecture, not features added after a multi-agent UI.

---
