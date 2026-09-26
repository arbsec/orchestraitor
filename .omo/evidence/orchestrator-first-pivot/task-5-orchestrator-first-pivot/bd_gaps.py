#!/usr/bin/env python3
"""The two arbsec/arbitraitor gap issues (ledger F15 substantiation).

Filed in arbsec/arbitraitor following ITS issue conventions
(.github/ISSUE_TEMPLATE/feature_proposal.yml: Problem / Proposed solution /
Alternatives considered). Labels: enhancement (+ security-sensitive for the
approval-path gap).
"""

GAP_ISSUES = [
    {
        "id": "GAP-HEADLESS-APPROVAL",
        "repo": "arbsec/arbitraitor",
        "title": "Headless/non-interactive ApprovalPrompt implementation (arbitraitor_mcp::ApprovalPrompt has only StdinApprovalPrompt)",
        "labels": ["enhancement", "security-sensitive"],
        "body": """### Problem

ADR-0013 (plan-bound approval capability model, Accepted) defines how approval
works, and `arbitraitor_mcp::ApprovalPrompt`
(`crates/arbitraitor-mcp/src/lib.rs:747`) is the pluggable prompt trait — but
the **only shipped implementation is `StdinApprovalPrompt`**
(`crates/arbitraitor-mcp/src/lib.rs:769`), which reads from stdin.

Headless embedders cannot use it. Orchestraitor's orchestration loop spawns
one-shot mini-agent workers without a TTY (fresh-context workers per its spec
§9.33.3; `approval-required` is a stable, lease-protected lifecycle state with
checkpoint resume per its §9.24/§9.9). When such a worker hits an
approval-demanding operation, there is no non-interactive prompt
implementation to substitute: the default prompt either blocks on a closed
stdin or fails.

Reproduce (from a consumer's seat):

1. Construct an `McpServer` with the default `StdinApprovalPrompt` wiring.
2. Run a tool call that requires approval with stdin closed / not a TTY
   (e.g. under a daemon or CI process).
3. The approval path cannot render or read a prompt; there is no headless
   `ApprovalPrompt` implementation in the crate to inject instead.

### Proposed solution

Ship a headless/non-interactive `ApprovalPrompt` implementation in
`arbitraitor-mcp` (or a small family of them), e.g.:

- a pending-approval store + explicit resolution API (persist the approval
  request as durable state; a trusted UI path resolves it later), or
- a callback/channel-based prompt the embedder supplies at construction.

It must preserve the ADR-0013 invariants: approval binds to the canonical
execution plan, and the agent that proposes a command can never manufacture
or confirm the approval for it (H-11). The trusted-UI rule (approval belongs
to the trusted UI, never agent-generated text) must hold on the headless path
just as it does on stdin.

### Alternatives considered

- **Each embedder implements the trait privately** — rejected: approval
  authority belongs to Arbitraitor; N private reimplementations of a
  security-sensitive surface is exactly the divergence ADR-0038 exists to
  prevent.
- **Orchestraitor avoiding approval-demanding operations entirely** —
  rejected: fail-closed mediation is the point of the integration; the loop
  needs a legitimate way to surface `approval-required` headlessly.

Context: filed from Orchestraitor's orchestrator-first backlog creation
(research ledger F15: "no headless/non-interactive approval path beyond
pluggable ApprovalPrompt trait (only StdinApprovalPrompt shipped)"). The
consuming Orchestraitor task is linked to this issue via a cross-repo
`blockedBy` edge and carries `blocked:arbitraitor`.
""",
    },
    {
        "id": "GAP-STABLE-EMBEDDING",
        "repo": "arbsec/arbitraitor",
        "title": "Land ADR-0038 pipeline-engine extraction (arbitraitor-engine) to give embedders one stable pipeline surface",
        "labels": ["enhancement"],
        "body": """### Problem

ADR-0038 (`docs/adr/0038-pipeline-engine-crate-extraction.md`, **status:
Proposed**, 2026-07-23) documents that three independent compositions of the
fetch→store→analyze→provenance→receipt→verdict pipeline exist today:

1. `arbitraitor-cli/src/pipeline.rs` (per ADR-0027) — fetcher, ContentStore,
   AnalysisCoordinator, signature verification, receipt building; **no policy
   evaluation or release**.
2. `arbitraitor-mcp` tool handlers — per-call fetch+analyze / fetch-only /
   analyze-only; **no ContentStore, PolicyEngine, or receipt building**.
3. `arbitraitor-daemon::ArbitraitorApi` — fetch+store+analyze+policy+receipt;
   **no provenance verification**.

Each composition covers a different subset — a consumer switching between
surfaces (CLI, MCP, daemon) gets different security coverage without knowing
it (silent coverage holes, per the ADR's own words). Third-party embedders
(Orchestraitor among them) currently have no stable embedding API: the
in-process `ArbitraitorApi` leaks internal types and is not the specced
stable surface, so Orchestraitor's tech-stack note pins it to the individual
crates directly until this lands.

### Proposed solution

Accept and land ADR-0038:

- extract the `arbitraitor-engine` crate owning the consolidated pipeline;
- unify the three compositions onto it (closing the silent coverage holes);
- publish the engine-owned `InspectionResultReceipt` wrapper (ADR-0038
  decision 6) so embedders stop depending on raw `arbitraitor-receipt` types;
- expose the stable embedding surface the spec describes (library, daemon,
  MCP gateway — all over a single pipeline engine).

### Alternatives considered

- **Orchestraitor pins `ArbitraitorApi` directly** — rejected: it leaks
  internal types, diverges from the specced surface, and would need rework at
  extraction time.
- **Shelling out to the CLI** — rejected: ADR-0038's context names embedding
  (not subprocess) as the intended path for third-party products.

Context: filed from Orchestraitor's orchestrator-first backlog creation
(research ledger F15: "no stable embedding API (ADR-0038 Proposed; 3 divergent
pipeline compositions in CLI/daemon/MCP)"). The consuming Orchestraitor task
is linked to this issue via a cross-repo `blockedBy` edge and carries
`blocked:arbitraitor`.
""",
    },
]
