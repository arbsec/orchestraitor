# Orchestraitor specification: Orchestration and autonomous delivery

### 9.24 Task and session lifecycle

This subsection defines the protocol-neutral internal state machine for tasks and sessions. It MUST NOT be coupled directly to MCP, ACP, A2A, or any one provider API.

#### 9.24.1 States

```text
queued                # task admitted to the controller; not yet scheduled
running               # worker picked up the task and is processing
input-required        # worker paused awaiting user input (typed, not security approval)
approval-required     # worker paused awaiting Arbitraitor-originated approval (§9.9)
authentication-required # worker paused awaiting user-side credential resolution
paused                # user/admin explicit pause; resumable
completed             # task finished successfully; receipts written
failed                # task terminated abnormally; partial results may exist
cancelled             # cancellation propagated; resources released
rejected              # Arbitraitor refused the plan; never ran; receipt emitted
orphaned              # process exit detected with task state still in `running`; recovery pending
```

Orphaned is an explicit recovery state, not a silent failure. The controller MUST detect orphaned sessions (worker process gone, task still marked running) within a configurable heartbeat interval (default 30 s) and transition to either `failed` (with partial-result preservation) or `paused` (awaiting reconnect) per policy.

#### 9.24.2 Cancellation, recovery, leases, idempotency

- **Cancellation propagation**: a user/admin cancel initiates a bounded grace period; workers receive a cancellation token; on expiry the controller reaps the worker process and its sandbox. Everything still in-flight (open file handles, network sockets, child processes) is the responsibility of Arbitraitor's resource-release path (§9.6 effective controls + §9.12 network) — Orchestraitor records "could not be stopped" in the audit log when the reaper fails to clean a handle.
- **Crash recovery**: durable task state (queued/running/paused/completed/failed/cancelled/rejected) survives a daemon restart. On restart, `paused` tasks stay paused; `running` tasks transition to `orphaned`; `approval-required` and `input-required` tasks stay where they are.
- **Checkpoints**: long-running tasks SHOULD emit periodic checkpoints (e.g., after every N tool calls or after a configurable time budget). Checkpoints enable replay-from-checkpoint without re-running prior tool calls.
- **Reconnect/resume**: a session that lost its worker can reconnect to a fresh worker and resume from the latest checkpoint. Provider-side conversation state is preserved by the harness's request log; on resume, Orchestraitor replays the model-side context window per the conversation transcript and resumes the state machine.
- **Leases and TTLs**: every task carries a lease (default 1h, configurable per session/domain/role). Lease expiry transitions to `orphaned` (not direct `failed`) so the user can extend.
- **Orphan-process cleanup**: a periodic reaper (default interval 30 s) walks running tasks; tasks whose worker process has exited without a clean transition move to `orphaned`. Orphaned processes' resources are released via Arbitraitor's resource-release path; Orchestraitor records the unreleased set.
- **Partial-result preservation**: a task that fails or is cancelled MUST preserve partial results (partial patches, completed tool calls, model responses) in the session's event store. The user can see them via the TUI and may promote partial patches via the §9.14 output quarantine.
- **Idempotency controls**: every state transition carries a stable operation ID + an idempotency key. Replays of the same operation MUST be no-ops in the state machine (worker retries are still possible; the state machine itself is replay-safe). Idempotency for side-effecting operations (tool calls, file mutations) MUST be proven by the tool, never assumed (see §9.26).

### 9.25 Principal identity and delegated authority

Every actor in the system carries a stable principal identity:

| Principal type | Identity source |
|---|---|
| user | OS user + stable Orchestraitor user id (stored in user config) |
| session | `SessionId` (per spec §18.2) |
| agent | domain id + role id + spawn generation |
| subagent | parent agent id + spawn generation |
| plugin | plugin identity from `arbitraitor_plugin_api::PluginIdentity` (publisher + version + trust class) |
| MCP server | `<server_id>` + executable SHA-256 (local) or TLS cert SPKI hash (remote) |
| remote worker | (parallel session — same trust model as local worker, narrower capability grants) |

#### 9.25.1 Delegation chain

Every security-sensitive action (tool call, file mutation, network request, secret use, output promotion) MUST carry a delegation chain recorded in the event:

```text
user:alice
  -> session:sess_7e3f
    -> agent:frontend:implementing:gen_4
      -> tool:fs.apply_patch
        -> arbitraitor:ApprovalTokenIssuer.issue(plan_ctx=...)
```

The chain is what an auditor follows from "what happened" to "who authorised it". The chain is preserved across leases, checkpoints, and reconnect/resume.

#### 9.25.2 Short-lived, resource-scoped authority

Authority MUST be scoped and short-lived rather than shared ambient credentials:

- a session should never inherit the user's ambient credentials (env vars, keyring entries, default SSH keys); they are released per-call by the Arbitraitor secret broker (§9.13);
- a subagent inherits ONLY the explicit capabilities granted by its manifest + the parent agent's grants — never the parent's full authority by default;
- the brokered credentials used by a worker (e.g., a scope-limited GitHub push token) MUST be issued with an expiry aligned to the operation's lease; the broker revokes on lease expiry.

#### 9.25.3 Authority issuance and validation belong to Arbitraitor

Per §2.2 + §9.9 + §9.13 + §16, Orchestraitor assembles a `PlanContext` and submits it via `arbitraitor_mcp::ApprovalTokenIssuer`. Orchestraitor DOES NOT issue capability tokens, validate signatures on approval tokens, or authorize secret release. Attribution, revocation, audit, and non-repudiation receipts all originate from Arbitraitor and are presented to the user via the Orchestraitor TUI/CLI.

### 9.26 Retry and failure semantics

#### 9.26.1 Transient failures only

Retry applies ONLY to classified transient failures:

- transport timeouts, connection resets, 429 with retry-after, 5xx (except 501); provider 5xx with retry-after headers;
- Arbitraitor placement of operations into arbitration-required (approvals) or unsupported categories is NOT a retryable failure — the user must resolve the gap in Arbitraitor.

Classifications rule out retry on:

- network request that returned success (worker side-effecting, response not "transient" just because content was wrong);
- side-effecting tool call (unless the tool proves idempotency, see §9.26.3);
- approval/plan validation failures — re-submission requires a new `PlanContext`;
- invalid model id (terminal, not transient).

#### 9.26.2 Bounded exponential backoff with jitter

- base 200 ms; factor 2; cap 30 s; jitter ±20%;
- retry budget: default 5 attempts per operation, configurable per session/domain/role/provider;
- circuit breaker per provider (open after N consecutive failures within a window; half-open after cooldown; closed after success);
- cancellation-aware: if the user/parent cancels during backoff, the retry loop MUST terminate immediately and not be hidden by the backoff timer.

#### 9.26.3 Side-effecting operations must prove idempotency

Side-effecting operations (tool calls, `fs.apply_patch`, `format.run`, process execution, network POSTs) MUST NOT be retried unless the operation carries a proven-idempotency marker:

- `fs.apply_patch` is idempotent by optimistic-concurrency digest (per §9.5) only if `expected_digest` matches at retry time;
- network POSTs are NOT idempotent by default; provider APIs that follow HTTP semantics (`PUT`, `DELETE`) are treated as idempotent; `POST` requires an explicit `idempotency-key` header (cloud APIs support this);
- Arbitraitor-owned operations (`arbitraitor_mcp::request_approval`, `arbitraitor_mcp::run_approved_artifact`) follow the plan-bound token model (ADR-0013) — tokens are single-use; retries require a fresh token with a fresh `PlanContext`.

#### 9.26.4 Partial-stream preservation, fallback, attribution

- Partial streams (e.g., incomplete SSE chunks before a connection drop) MUST be preserved in the event store with a `partial: true` marker;
- Per-call usage records (token counts, elapsed time, cost accrued so far) MUST be recorded even on failure (per §9.19.4);
- Failure attribution MUST resolve to the layer at fault (transport, provider, model, adapter, tool, Arbitraitor, user-cancellation, lease-expiry);
- Fallback across providers/models is configurable per route (§9.19.2) and visible in the per-call event — never silent. Soft-cap fallback (§9.19.5) is also a fallback trigger.

### 9.27 Resource governance

#### 9.27.1 Configurable per-session and per-task limits

Configurable limits per session or per task (insertable at any §9.22.2 layer):

- concurrency (max active worker processes, max parallel tool calls);
- subprocesses (max spawned, max recursion depth);
- CPU (cores, CPU seconds);
- memory (resident set cap);
- disk (workspace size, output size, log retention);
- files (max open, max created);
- output (stdout/stderr byte cap, tool-output size cap — already in spec §9.5);
- network (per-destination request rate, byte cap);
- model calls (per session, per agent, per minute);
- tokens (input + output + reasoning);
- spend (per §9.19.5-§9.19.6 budgets).

#### 9.27.2 Backpressure and fair scheduling

- Backpressure: when a session hits a soft limit, the controller queues new operations rather than dropping them. Failure to drain a queue within a configurable timeout surfaces as a `paused` task (§9.24) awaiting user action.
- Fair scheduling: when multiple agents share a worker pool, the controller applies fair-share scheduling (configurable policy; default: round-robin per agent, weighted by computed priority — see §9.27.3).

#### 9.27.3 Orchestration vs. security enforcement — boundary

Orchestration limits (the above) are enforced by Orchestraitor's controller and recorded in the event store. **Security-enforced limits** (effective resource caps that the worker cannot bypass) MUST come from `arbitraitor_plugin_api::CapabilitySet` (max_memory_bytes, max_cpu_ms) + Arbitraitor's sandbox resource limits (§9.6). When the controller's orchestration limit is tighter than Arbitraitor's enforced cap, the controller's value is used (the worker never sees the looser cap). When the controller's value is looser, the Arbitraitor cap wins and the controller's looser value is silently lowered — recorded as `clamped by Arbitraitor capability` in the event.

#### 9.27.4 Cancellation releases resources promptly and visibly

Cancellation MUST release:

- file descriptors, network sockets, child processes (reaped via process group);
- Arbitraitor-issued capability tokens (lease expiry + explicit revoke per §9.25);
- workspace snapshots for cancelled sessions (after a configurable retention window to allow post-mortem);
- in-flight model requests (transport cancellation token).

Anything that could not be released cleanly is recorded in the event log with the resource type and the reason; the user is notified via the TUI.

### 9.33 Spec-driven autonomous delivery

Orchestraitor supports a configurable autonomous delivery workflow as its primary product axis: this workflow is the self-improving orchestration loop (see [`00-overview.md` §1](00-overview.md#1-executive-summary)), and the live loop on the shared board is the M1 delivery milestone (see [`60-milestones.md`](60-milestones.md)). The workflow converts a specification into merge-ready changes through isolated implementation, verification, and adversarial review — all within Arbitraitor's security boundary.

The default workflow:

```text
specification
  -> task decomposition
  -> backlog approval
  -> task scheduling
  -> isolated implementation
  -> verification
  -> change set or PR
  -> adversarial review
  -> remediation
  -> merge-ready result
  -> next eligible task
```

All thresholds, roles, routing, concurrency, stop conditions, retry behavior, review selection, and escalation rules MUST have sane defaults and be configurable through the same layered configuration system (§9.22). Arbitraitor remains the exclusive owner of security policy, capability enforcement, sandboxing, approvals, privileged operations, provenance, and receipts (§2.2).

#### 9.33.1 Roles

Configurable agents or profiles for the delivery workflow. These are defaults, not hardcoded agents — users MAY combine, replace, rename, disable, or customize them (per §9.22.4 — no hardcoded task taxonomies).

| Role | Responsibility |
|---|---|
| `spec-author` | Collaborates with the user to create or revise a specification. |
| `task-planner` | Converts an approved spec into a dependency-aware task DAG. |
| `project-manager` | Selects eligible tasks, assigns agents/models, tracks state, manages retries. |
| `implementer` | Performs one bounded task in a fresh context and isolated workspace. |
| `reviewer` | Performs independent review in a fresh context. |
| `domain-reviewer` | Reviews relevant areas (security, backend, frontend, data, DevOps, testing, documentation). |
| `verifier` | Runs and interprets required checks where separated from implementation. |

Roles map to `(domain, role)` pairs in the §9.19.1 agent catalog. The `project-manager` role is a new built-in role alongside §9.19.1's `planning`/`implementing`/`reviewing`/`testing`/`researching` — it's the autonomous orchestration driver.

#### 9.33.2 Task generation

Tasks created from a specification MUST include:

```text
stable ID
spec requirement references         # traceability from spec → task → implementation → review → evidence → changes
title and objective
acceptance criteria
dependencies                         # DAG edges
domain and risk classification      # maps to §9.19.1 domain + §9.28 data sensitivity
expected files or components
required verification                 # which checks must pass (spec §21.10 CI items)
required reviewer domains            # which §9.33.1 domain-reviewer roles must review
autonomy level                       # full | guided | manual
model/agent routing                  # per §9.19.2 precedence chain
retry policy                         # per §9.26
completion evidence                  # what proves the task is done
```

Prefer thin vertical slices. Preserve traceability from spec requirement to task, implementation, review findings, verification evidence, and final changes.

The user MUST be able to review and edit the generated backlog before autonomous execution begins. `orc backlog show` displays the DAG; `orc backlog approve` starts the autonomous run.

#### 9.33.3 Autonomous backlog execution

The project-manager agent MAY continue until:

- the backlog is empty;
- no task is currently eligible;
- a configured budget or time limit is reached;
- an approval or user decision is required;
- repeated failures exceed policy;
- a security invariant or organization policy blocks progress;
- the user pauses or cancels the run.

Only dependency-satisfied tasks MAY start. Parallel execution MUST respect configurable concurrency, repository conflicts, resource budgets (§9.27), provider limits (§9.19.5-§9.19.6), and review capacity.

Each task receives a **fresh, minimal context** derived from: the spec, task metadata, relevant project knowledge (via the context compiler §9.15), current workspace state, and dependency outputs. Do NOT pass accumulated conversations between agents — fresh context prevents accidental authority leakage and context poisoning (§7.3).

#### 9.33.4 Change-set and PR review loop

Completion of an implementation change set triggers a configurable review pipeline. A GitHub PR is one trigger, but local branches or staged change sets MUST also work without GitHub.

Reviewer selection is based on:

- changed files and symbols;
- languages and frameworks;
- task domains;
- dependency and configuration changes;
- risk classification;
- Arbitraitor findings;
- project policy.

Example selection:

```text
general reviewer
  + security reviewer for auth, permissions, dependencies, CI, scripts, or execution
  + backend reviewer for service/API changes
  + frontend reviewer for UI changes
  + testing reviewer when coverage or verification changed
```

Reviewers MUST use fresh contexts (new `(domain, role)` agent spawn per §9.25) and MUST NOT be the same agent session that implemented the change.

Each finding MUST include: severity, evidence, affected paths, violated requirement or rule, and proposed remediation. Findings MUST be deduplicated and tracked across loops.

The review loop is configurable:

```text
max_review_loops                    # default: 3
max_reviewers                        # default: 5
required_reviewer_domains            # default: ["security"] for security-sensitive tasks
minimum_severity_to_block            # default: "high"
allow_same_model                     # default: false
require_provider_diversity           # default: false
require_human_review                 # default: false (true for security-sensitive changes)
stop_when_no_blocking_findings       # default: true
```

Typical loop:

```text
review
  -> consolidate findings
  -> assign remediation
  -> implement fixes in fresh context
  -> verify
  -> review again
```

Stop when no blocking findings remain or a configured limit is reached. Reaching the limit MUST produce an explicit `blocked` or `needs-human` state (per §9.24 lifecycle state machine), never silently approve the result.

Security-sensitive changes MUST require Arbitraitor checks (§9.9 approval, §9.14 output promotion) and MAY require mandatory human review regardless of automated reviewer output (per §21.1 — "Changes to privileged brokers, sandboxing, policy enforcement, capability issuance, filesystem projection, network controls, secret handling or unsafe code require human review before release").

#### 9.33.5 Failures and retries

Persist every error with:

```text
task and attempt ID
phase                                 # decompose / implement / verify / review / remediate
agent/model/provider
normalized error class
retriable status
workspace generation
relevant logs and evidence
partial results
next retry time
```

Classify failures before retrying (extending §9.26.1):

```text
transient provider or network failure     # retryable, bounded backoff
rate limit                                 # retryable, honor retry-after
tool or process failure                    # retryable if idempotent
verification failure                       # NOT retryable blindly — fix the root cause
merge conflict                             # NOT retryable blindly — resolve conflict first
invalid agent output                       # re-prompt with fresh context, limited retries
policy denial                              # NOT retryable — resolve in Arbitraitor
approval required                          # NOT retryable — await user action
non-retriable configuration or security failure  # NOT retryable — escalate
```

Use configurable bounded exponential backoff with jitter for transient failures (per §9.26.2). Support per-task, per-phase, provider, and global retry budgets.

NEVER blindly retry side-effecting actions. Resume from checkpoints (§9.24.2) or retry only when the operation is idempotent or its previous effects have been safely rolled back (per §9.26.3).

Repeated implementation or verification failures MAY trigger escalation:

```text
same agent with fresh context
  -> alternate model
  -> domain expert
  -> revised task plan
  -> human escalation
```

Do NOT retry policy denials, missing approvals, or non-retriable security failures as though they were transient errors.

#### 9.33.6 Durability and control

The orchestration state MUST survive daemon restarts (per §9.24.2 crash recovery) and include:

- task DAG and backlog state;
- assignments and attempts;
- workspace and checkpoint references;
- review loops and findings;
- verification evidence;
- retry schedules;
- budgets and costs (per §9.19.4-§9.19.6);
- outstanding approvals;
- Arbitraitor receipts.

Provide controls: `pause`, `resume`, `cancel`, `reprioritize`, `retry`, `skip`, `manual-assignment`. Exposed via `orc backlog pause|resume|cancel|retry|skip|assign` CLI commands and the TUI session dashboard.

All thresholds, roles, routing, concurrency, stop conditions, retry behavior, review selection, and escalation rules MUST have sane defaults and be configurable through the §9.22 layered configuration system (no hardcoded per §9.22.1).

#### 9.33.7 Security boundary

Arbitraitor remains the exclusive owner of security policy, capability enforcement, sandboxing, approvals, privileged operations, provenance, and receipts (§2.2). The autonomous delivery workflow is Orchestraitor's orchestration concern — it schedules, isolates, verifies, and reviews, but it MUST NOT:

- make security decisions (allow/deny/verdict);
- bypass Arbitraitor approval requirements;
- skip output promotion (§9.14) for security-sensitive file classes;
- silently weaken any security control to make a task pass;
- bypass the §9.22.9 explicit/visible/auditable rule for security-weakening.

The `project-manager` role assigns tasks and retries — but capability grants, workspace isolation, network enforcement, and secret brokering all come from Arbitraitor per the §9.25 principal-identity + delegation-chain model.

#### 9.33.8 Design principles

> Specifications define intent, tasks define bounded work, and fresh contexts prevent accidental authority and context leakage.

> Autonomous execution may continue without supervision, but it must always have explicit budgets, stop conditions, durable state, and auditable evidence.

> An empty backlog is success. A blocked backlog is a visible state requiring resolution, not an excuse to loop forever.

---

### 9.34 Structured error taxonomy

The UX requirement for good error messages is backed by a typed error model, not only good copywriting. Every error surfaced to the user or an agent MUST be a structured error carrying:

```text
stable error code         # e.g., ORC-WORKSPACE-004
human-readable cause      # plain-language explanation
affected component        # which crate/subsystem produced the error
retryability              # retriable | not-retriable | needs-user-action
suggested action          # one or more concrete next steps
relevant configuration      # which config key(s) are relevant, if any
log or trace reference     # correlation ID for tracing
underlying source chain    # the causal chain from thiserror::source
```

Example:

```text
ORC-WORKSPACE-004

The workspace could not be promoted because src/auth.ts changed
outside Orchestraitor after this task began.

Options:
  Compare changes     orc compare <base> <current>
  Rebase the staged transaction
  Restore the external version
  Cancel promotion
```

Error codes follow the pattern `ORC-<COMPONENT>-<NNN>` where `<COMPONENT>` is the crate short-name (`WORKSPACE`, `PROVIDER`, `MCP`, `DAEMON`, `CONFIG`, `DELIVERY`, `SANDBOX`, etc.) and `<NNN>` is a zero-padded number. Codes are stable across versions — deprecation replaces a code with a new one but does not reuse the old number. The code registry lives in `orchestraitor-model` as atyped enum with `#[derive(strum::EnumString, strum::Display)]` so it round-trips through serde.

Reserve truly generic messages for unexpected internal faults; even those MUST include a correlation ID and a bug-report command (`orc bug-report --correlation-id <id>`).

The error taxonomy is implemented in `orchestraitor-core` and consumed by the CLI (via `miette`'s `Diagnostic` trait), the TUI (rendered in the error panel), and the daemon (serialized as JSON-RPC error objects). Errors never contain secrets, headers, cookies, signed URLs, or approval tokens (per Arbitraitor `conventions.md:92-98` + §9.23.4 trace redaction rule).

---

### 9.35 Campaign orchestration: session-per-decision

The orchestration loop runs as a sequence of short-lived campaign sessions. A campaign session is a fresh, bounded manager invocation: it reads the reconciled board state (§9.43), applies the epic-focus scheduling policy (§9.41), selects at most one eligible unit of work, persists one decision record, and exits. There is no long-lived orchestrator conversation — no accumulated conversation state, no in-memory orchestration context, and no decision made outside a recorded session. This extends the §9.33.3 fresh-context rule to the orchestrator itself: the manager is as fresh as the workers it spawns, which prevents context poisoning and accidental authority leakage (§7.3).

Every campaign session persists exactly ONE decision record before exiting:

```text
decision id + campaign session id
kind                     # spawn | no-op
selected task            # stable board item identity (§9.43)
role                     # orchestration role from the role registry
model + provider         # resolved per §9.19.2 precedence
worker arguments         # task metadata, workspace spec, capability requests
rationale                # why this task, this role, this model
alternatives considered  # with per-alternative skip reasons
```

A spawn decision hands the record to the watch daemon (§9.36), which executes it — the campaign session never spawns workers itself. A no-op decision MUST carry a typed reason, and the reasons are distinct:

```text
empty-queue     # no eligible work exists at all
all-blocked     # eligible work exists but every candidate is blocked; the blocked graph is attached
epic-exhausted  # the active epic has no remaining schedulable work (§9.41)
```

Campaign sessions hold no state between invocations: everything they read is durable (board state plus local run state per §9.43) and everything they write is the decision record. A crashed campaign session is safe by construction — the next poll tick runs a fresh one; there is no partial-decision recovery to perform. Campaign sessions run under the same §9.24 lifecycle, §9.25 principal identity, and §9.27 resource governance as any other session, and every tool they use is a coordinator decision tool (§9.39) mediated by Arbitraitor.

### 9.36 Watch daemon

The watch daemon (`orcd watch`) is the always-running supervision loop for §9.35 campaigns and their workers. It owns no orchestration decisions — decisions come from campaign sessions — it executes and supervises them.

- **Poll tick.** The daemon polls the board provider on a fixed default cadence, operator-configurable through the §9.22 layered configuration and adapted to provider rate-limit feedback (§9.43). Every tick is a reconcile pass: board-wins sync with `board-diverged` events, promotion of newly unblocked tasks (§9.40), and re-evaluation of kick-off conditions.
- **Kick-off conditions.** The daemon spawns a campaign session when none is in flight and at least one condition holds: new eligible work appeared, a blocked task became unblocked, a lease or worker slot was released, or a budget window reset. Conditions are configurable; the daemon never nudges a human and never bypasses a budget to keep the loop moving.
- **Stall and orphan detection.** The daemon enforces §9.24 leases and TTLs: heartbeats are local, lease expiry transitions to `orphaned` (never direct `failed`), and the reaper walks running tasks on its configured interval. A campaign session that produces no decision record within its lease is orphaned and re-run fresh; a stalled worker is detected the same way.
- **Budget enforcement.** Before any spawn the daemon enforces three budget classes: monetary spend (§9.19.6), run/time budgets per task and per campaign, and subscription-usage budgets ([§9.46](30-model-routing.md#946-subscription-aware-routing)). Values are operator-configured; the spec fixes the classes and the failure behavior — a budget stop produces an explicit `blocked` or `needs-human` state (§9.33.4), never a silent skip and never a weakened retry.
- **Lifecycle mapping.** Everything the daemon does maps onto §9.24 lifecycle states and §9.26 retry semantics: worker crashes are `orphaned` transitions, transient provider failures follow bounded backoff, and non-retriable classes (merge conflict, verification failure, policy denial) never loop. The daemon is itself crash-safe: on restart it resumes from durable state per §9.24.2 — `paused` stays paused, `running` becomes `orphaned`, and the poll tick resumes.

The daemon runs unprivileged; foreground and systemd-user-unit supervision are documented operating modes, not requirements.

### 9.37 Agent issue reporting

Agents in the loop — workers, campaign sessions, reviewers, verifiers — MAY report bugs and issues they encounter. Reporting is reporting ONLY: filing an issue never grants work, schedules work, or implies that the reporter will fix it.

**(a) Failure-driven auto-filing.** When a task exhausts its attempt budget on a reproducible failure, the loop files a Bug at Status=Triage with structured failure context — normalized failure class (§9.33.5), evidence pointers, task and session links — alongside the task's `blocked`/`needs-human` transition. The Bug is the durable record of the exhaustion; the task never silently retries past its budget.

**(b) Discretionary reporting.** Agents have a `report_issue` coordinator tool (§9.39) for issues noticed but not task-fatal: structured content (title, class, description), evidence links, and affected paths. `report_issue` files at Status=Triage ONLY — triage is a human/PM gate. A reported issue is never auto-scheduled, never promoted to Ready by the reporting path, and never assigned to its reporter.

**(c) Routing.** Reported issues follow the ownership table (workflow policy): security-relevant Arbitraitor gaps keep the `blocked:arbitraitor` upstream flow — the issue is filed in `arbsec/arbitraitor`, the Orchestraitor task stays blocked with the `blocked:arbitraitor` label, and no local workaround ships in its place (§16.2).

**(d) Untrusted content.** Agent-authored issue bodies are untrusted content (§6.1): marker-wrapped and injection-hardened — the same boundary Arbitraitor's `sanitize_for_agent` applies when untrusted text is quoted to downstream agents — and they never carry instructions any agent executes without policy review.

**Report ≠ self-fix.** The reporting agent does NOT pick up its own reported bug; the default is file-and-continue or end-task, and scheduling a reported Bug is the PM/human triage path. SINGLE EXCEPTION: a defect that BLOCKS the reporting agent's current task is fixed in that task's own PR with a regression test — correctness defects needed for safe completion are never deferred merely to shrink a PR. If the fix is too large for the in-flight slice, the Bug becomes a `blockedBy` dependency (§9.40) that the normal loop schedules before the blocked task resumes.

### 9.38 MCP-early tool strategy and the built-in MCP proxy

Worker search and code-intelligence needs are satisfied from day one by approved, fingerprinted MCP servers rather than by native indexing built first: a codegraph-style symbol/call-graph server and a codebase-memory-style knowledge-graph server cover the explore and search workload while the native indexing paths (§9.15, §9.16) mature. This pulls the knowledge-index federation originally scoped for post-MVP forward into the bootstrap.

Every such server is an untrusted principal under §9.18.1 containment: fingerprinted per session (executable digest or TLS identity, per-tool schema digests, declared vs. granted capabilities), launched through Arbitraitor inspection, and contained in an Arbitraitor-reported sandbox (`arbitraitor_sandbox::SandboxMode::Restricted` minimum). MCP annotations remain advisory input to policy; authority over destructive vs. non-destructive vs. idempotent comes from Arbitraitor's analyzer, never from the server's claim.

**Built-in MCP proxy.** Orchestraitor ships a first-class MCP PROXY: agents get ONE endpoint that fronts (1) the MVP-6 built-in tool surface, (2) approved external MCP servers, and (3) Arbitraitor's own MCP server. The proxy is a routing and policy-surface layer ONLY — tool namespacing, fingerprint pinning, schema-drift detection, and per-tool policy presentation. It is NEVER a security authority: it makes no allow/deny/verdict decisions, issues no capabilities, and enforces nothing Arbitraitor does not enforce (§2.2). A tool call through the proxy crosses the same Arbitraitor boundary as a direct call; proxying MUST NOT widen any grant. (mcproxy-go is a pattern-reference for transport aggregation only; the security posture is Orchestraitor's own.) Tool results surfaced to agents through the proxy are untrusted content and MUST pass Arbitraitor's `sanitize_for_agent` boundary when quoted into agent-facing context.

### 9.39 Coordinator decision tools

Agents get a first-class INTERNAL decision-tool surface, exposed through the same built-in tool layer as the MVP-6 coding tools but for orchestration rather than file operations:

```text
board.query        # read board state: items, statuses, fields, dependency edges, blocked graph
board.move         # guarded board transitions (status/field writes): validated, lease-checked,
                   # reconcile-visible — never a raw provider write
decision.record    # persist a decision record with rationale (§9.35); append-only, replayable
router.consult     # ask the model router for a (provider, model) resolution with alternatives
worker.delegate    # spawn a worker with scoped authority (§9.25.2): explicit capability requests,
                   # lease-aligned expiry — never the parent's full authority
budget.check       # query spend, run/time, and subscription budget state (§9.36)
capability.check   # query Arbitraitor's capability report for a requested operation shape
```

These tools are tools: every invocation is Arbitraitor-mediated like any other tool call — assembled into a `PlanContext` and authorized via `arbitraitor_mcp::ApprovalTokenIssuer` where the operation is security-sensitive (§9.25.3), evaluated by Arbitraitor's `PolicyEngine`, recorded in the event store with the §9.25.1 delegation chain — never a silent authority grant. `worker.delegate` scopes down, `board.move` refuses transitions the provider or workflow policy rejects, and `capability.check` reports Arbitraitor's answer rather than creating one. Router consultation resolves through the role registry ([§9.45](30-model-routing.md#945-role-based-model-routing)).

Tool arguments are untrusted input (§6.1). Injection-boundary negatives are mandatory test surface: a tool argument containing instructions, marker-escape attempts, or content addressed to a different principal MUST be treated as data — quoted, sanitized, never executed. Classification of decision payloads (what may leave the machine, what is security-sensitive) stays Arbitraitor's job (§9.28.4); the tools carry structured, typed content only.

### 9.40 Blocked-dependency semantics

Native `blockedBy` edges on the board ARE the dependency graph. Orchestraitor MUST NOT maintain a mirrored second DAG: the board's edges are the truth, the ready-queue predicate is computed from them at each poll, and no shadow copy exists to drift.

- **Visibility.** A blocked task is invisible to the campaign until its last blocker lands; the next poll tick promotes it (§9.36). Promotion is a reconcile effect, not an event-subscription requirement.
- **Cycles.** A dependency cycle is board-data corruption from the loop's perspective: it produces a needs-human report naming the cycle's members. A cyclic task is never scheduled, never auto-broken, and never retried as though transient.
- **`blocked:arbitraitor`.** A cross-repo Arbitraitor blocker is a hard wait — never retried as a transient failure (§9.26.1: Arbitraitor placement into arbitration-required or unsupported categories is not retryable). While the hard wait holds, bug-flow and other active-epic work continue (§9.41); the blocked task stays blocked with its upstream link until the Arbitraitor issue resolves (§16.2).
- **Mid-execution blocks.** A worker that hits a block mid-execution transitions to a lifecycle state, not a failure: `approval-required` and `input-required` are stable, lease-protected states with checkpoint resume (§9.24.2). Merge conflicts and verification failures are typed non-retriable classes (§9.33.5) — fix the root cause, never blind-retry.
- **Wholly-blocked active epic.** When every remaining task of the active epic is blocked, the loop emits a needs-human signal and continues bug-flow (§9.41); it never silently switches epics.
- **Stale-blocker drift.** When a blocker reaches Done but the edge remains, reconcile surfaces the drift as a `board-diverged`-class event; resolution is a human/PM board action. The loop MUST NOT silently rewrite or drop dependency edges.

### 9.41 Epic-focus scheduling policy

The ready queue is epic-focused: exactly one Epic is active by default (`max_active_epics = 1`, configurable through the §9.22 layers). The queue promotes only tasks of the active epic, priority-ordered by the board's P0-P3 field, and only leaf Tasks and Bugs that satisfy the workflow policy's eligibility rules.

- **Bug preemption.** Standalone Bugs ALWAYS preempt epic work UNLESS the Bug is epic-linked — it belongs to the active epic's sub-graph or blocks its tasks. Preemption is a queue-ordering rule, not an interruption: in-flight work finishes per its lease.
- **Human exclusion.** The ready queue excludes items assigned to humans. The service-identity set — the GitHub App bot identity and any machine identity configured alongside it — is declared in configuration, not inferred: items assigned to a service identity are schedulable; items assigned to a human are not.
- **Focus controls.** `orc epic focus <epic>` switches the active epic; `orc epic pause` pauses focus — in-flight work finishes per lease semantics while new spawns are blocked; `orc epic resume` lifts the pause. Chat steering (§9.44) maps to the same controls.
- **Exhaustion and blockage.** An exhausted or wholly-blocked active epic produces a needs-human signal while bug-flow continues (§9.40). The loop NEVER silently auto-switches to another epic unless `auto_advance_epic` is explicitly configured; the default is a visible stop.

### 9.42 Multi-org workspaces and cross-project epics

Configuration defines a named WORKSPACE that owns multiple organizations and their projects — each project a board-provider instance (§9.43) with its own credentials, never a shared ambient token. One daemon run serves exactly one active workspace; switching workspaces is an explicit daemon-level action, not a per-request routing concern.

Projects in a workspace are CONNECTABLE. An Epic lives on an anchor board and may span multiple projects: its Tasks and Sub-tasks are targeted at specific projects through cross-board membership edges — the epic's sub-graph is the union of its member items across boards, not a copy of them.

Resolution is workspace-wide: epic-focus (§9.41) treats the active epic's cross-board sub-graph as one queue, `blockedBy` edges (§9.40) may cross boards within the workspace — a task in one project blocked by an issue in another — and bug-preemption applies to standalone Bugs on any board of the workspace. Credentials, rate limits, and reconcile ticks remain per provider instance (§9.43); only dependency, focus, and preemption resolution cross boards.

### 9.43 Kanban board abstraction

Planning and task tracking run against a pluggable `BoardProvider` contract:

```text
items            # tasks, bugs, epics, features: stable identity, type, title, body
statuses         # board columns / status field values
fields           # typed custom fields (priority, target, risk, size, ...)
dependency edges # native blockedBy edges, cross-board within a workspace (§9.42)
cross-references # links between items, including cross-repository links
search           # filtered queries over items and fields
```

**Providers.** GitHub Projects v2 is the first provider: GraphQL-backed, with org/project/field/option node IDs resolved at runtime and cached OUTSIDE the repository (never committed), and a poll tick that adapts to rate-limit feedback. A local sqlite provider serves CI and offline operation — tests never hit the network (the deterministic-simulator rule, §21.3). Further providers implement the same contract; GitHub is not required by the loop.

**Single canonical provider.** A deployment has exactly one canonical board provider per workspace — never dual-master. Work items live ON the provider: Orchestraitor keeps a local read-cache that is never authoritative, writes through, then refreshes. Runtime state — decisions, leases, heartbeats, events, receipts, budgets — is local-only, keyed by stable board item identity, and never synced to the board; the board sees only coarse status transitions.

**Reconcile.** Every poll tick is a reconcile with board-wins semantics: where local and board state disagree, the board wins, the divergence is recorded as a `board-diverged` event, and stale reads are stamped with their last-synced time.

**Switching.** Provider switching is an explicit `orc board import/export` migration, not a live mirror.

**Evidence.** Completion evidence is published as board-item comment summaries; full artifacts stay local and are exportable on demand (§9.17).

### 9.44 Operator chat mode

`orc chat` is the operator's conversational surface into the loop. It does two things: reports progress and drafts new work.

- **Progress reads are summaries of durable state.** Board state, campaign decisions, run state, budgets, and the blocked graph are read from their durable homes (§9.43, §9.35) — never from chat-context memory. A chat answer about state is a read-only view stamped with the same last-synced caveats as every other read.
- **Drafting lands on the board, never the schedule.** Chat may draft new Epics, Features, and Tasks; drafts are filed at Status=Triage — the human/PM triage gate — never silently scheduled. Drafting is not decomposition: decomposition into leaf sub-issues follows the workflow policy, and the PM selection gate stays intact.
- **Distinct authority profile.** Chat runs with a narrower write authority than a campaign session: progress queries are read-only; drafting writes board items at Triage only; chat output itself carries no execution authority. Steering — focus switch, pause, resume — maps to the same guarded controls as `orc epic focus/pause/resume` (§9.41): the operator's instruction is the command and executes through the normal control path; model-generated text never mutates state on its own.

`orc chat` is a CLI session backed by the same coordinator decision tools (§9.39) under this narrower profile; TUI and daemon surfaces expose equivalent views.
