#!/usr/bin/env python3
"""E0 leaf tasks (first half: T1-T6). Thin-slice markers + deepening pointers."""

from bd_common import BUDGET_BLOCK

TASKS = [
    {
        "id": "E0-T1",
        "epic": "E0",
        "title": "Task: bootstrap-identity — register the ORCHESTRATOR SERVICE IDENTITY (GitHub App `arbsec-agent`) + token-minting path",
        "risk": "Critical",
        "estimate": 3,
        "labels": ["needs-human-review"],
        "blocked_by": [],
        "body": """## Objective

Register the GitHub App service identity for agent-driven GitHub operations
(machine identity per ledger D11) and wire the token-minting path: manifest
flow per the runbook `.omo/drafts/github-app-setup.md`, slug `arbsec-agent`,
per-installation least-privilege permissions (issues: write, pull requests:
write, projects read+write for the org boards the daemon works on — nothing
else), private-key-minted installation access tokens with ~1h expiry (no
long-lived PAT in daemon config), and bot attribution for commits/PRs/comments.

## Thin-slice scope

THIN SLICE: App registration (owner-level UI action) + installation + the
token-minting path for the single shared board. Workspace multi-credential
generalization deepens in E1 (multi-org workspaces).

## References

- `docs/spec/10-orchestrator.md` §9.25.2 (short-lived, resource-scoped authority)
- `docs/spec/10-orchestrator.md` §9.41 (service-identity set for assignee exclusion)
- `docs/spec/40-arbitraitor-integration.md` §9.23 (secret resolution — `secret://keyring/<id>` / `secret://env/<VAR>` URIs, never committed plaintext)
- Runbook: `.omo/drafts/github-app-setup.md` (manifest flow, minimal permissions, installation, token minting, assignee caveat)
- Ledger D11 (rationale: least privilege, ~1h tokens, webhooks, rate-limit class, auditability, bot attribution)

## Acceptance criteria

- [ ] The GitHub App `arbsec-agent` exists with the manifest-flow permissions from the runbook (owner UI action; record the App ID in the runbook, never the private key)
- [ ] `orc config get github_app.slug` resolves `arbsec-agent` through the §9.22 layered configuration
- [ ] Token minting produces an installation token with expiry <= 1h; the private key resolves via a `secret://` URI (never a path in the repo)
- [ ] Assignee caveat documented: board Status + Orchestraitor run-state carry ownership; the ready-queue assignee-exclusion treats the App bot slug as the SERVICE identity (items assigned to humans are excluded)
- [ ] `cargo nextest run --workspace` green including new unit tests for token-expiry handling

## QA scenarios

- Happy: `orc config get github_app.slug` -> `arbsec-agent`; minted token authenticates a read-only installations call; evidence `.omo/evidence/<task-slug>/token-mint.txt`
- Failure: expired/absent private key -> minting fails closed with a `secret://` resolution error (no fallback to ambient owner auth without an explicit labelled fallback); evidence `.omo/evidence/<task-slug>/mint-fail-closed.txt`

## Non-goals

No machine user account; no assignee-field semantics for the bot (fields only accept user accounts); no webhook receiver (polling-first per D5).

## Security impact

Credential handling at the orchestration boundary: short-lived scoped tokens, keyring-backed private key, zero long-lived PATs. Risk=Critical, needs-human-review.

## Testing requirements

Unit tests for expiry/refresh logic; negative test: minting with a revoked/absent key fails closed. No live-GitHub CI dependency (fixtures).

## Documentation impact

Runbook `.omo/drafts/github-app-setup.md` updated with the actual App ID + permission list; `CHANGELOG.md` `[Unreleased]` entry; agent docs note (plan todo 6 owns the full mandate).

## Dependencies

None (first E0 task; everything needing a GitHub identity consumes it).

## Rollback implications

App registration is reversible (delete the App / uninstall); config keys are layered and removable via `orc config unset`.
""",
    },
    {
        "id": "E0-T2",
        "epic": "E0",
        "title": "Task: bootstrap-board — minimal GitHub Projects v2 board provider (read + status-write)",
        "risk": "Low",
        "estimate": 5,
        "labels": [],
        "blocked_by": [],
        "body": """## Objective

Minimal board provider for the bootstrap loop: read the ready-queue (leaf
Task/Bug, Target=MVP, Status=Ready, no unresolved blockedBy) from the shared
GitHub Projects v2 board and write Status field transitions. Org/project/
field/option node IDs resolved at runtime via GraphQL and cached OUTSIDE the
repository (never committed).

## Thin-slice scope

THIN SLICE: read + status-write only, single board, no reconcile views, no
workspaces. The full `BoardProvider` contract, sqlite provider, adaptive
rate-limit tick, and multi-org workspaces deepen in E1.

## References

- `docs/spec/10-orchestrator.md` §9.43 (kanban board abstraction — provider subset)
- `docs/spec/10-orchestrator.md` §9.40 (blockedBy edges are the DAG — queue predicate filters on them)
- `.agents/project/github-project.example.toml` (field/option names; node-ID caching convention)
- Ledger F27 (GraphQL mutation shapes: `addProjectV2ItemById`, `updateProjectV2ItemFieldValue`)

## Acceptance criteria

- [ ] `orc board ready --json` lists exactly the leaf Task/Bug items with Target=MVP, Status=Ready, and no unresolved blockedBy on the shared board
- [ ] A status write (`orc board move <item> --status "In Progress"`) round-trips: read-back shows the new Status
- [ ] Node IDs are resolved at runtime and cached under `$XDG_CACHE_HOME` (or `~/.cache`), never inside any repo
- [ ] `cargo nextest run --workspace` green; provider unit tests run against recorded GraphQL fixtures (no live network in CI)

## QA scenarios

- Happy: `orc board ready --json` against the live shared board returns the expected queue shape; evidence `.omo/evidence/<task-slug>/ready-live.txt`
- Failure: GraphQL auth/rate-limit error surfaces as a typed error (no unwrap/panic); a fixture with a malformed item body is skipped with a warning, not a crash; evidence `.omo/evidence/<task-slug>/malformed-fixture.txt`

## Non-goals

No field writes beyond Status; no dependency-edge writes; no local sqlite provider; no reconcile/stall views.

## Security impact

Read-mostly surface; board content is untrusted input (§6.1) — item bodies are never executed or interpolated into commands.

## Testing requirements

Fixture-based unit tests (recorded GraphQL responses); one live smoke exercised manually with evidence captured.

## Documentation impact

`docs/cli/` entry for `orc board ready` / `orc board move`; CHANGELOG `[Unreleased]`.

## Dependencies

None (consumes E0-T1's identity when writing; reads work under owner auth until then).

## Rollback implications

Pure addition; status writes are board-field transitions a human can revert on the board itself.
""",
    },
    {
        "id": "E0-T3",
        "epic": "E0",
        "title": "Task: bootstrap-routing — heuristic static role→model routing table",
        "risk": "Low",
        "estimate": 3,
        "labels": [],
        "blocked_by": [],
        "body": """## Objective

Static heuristic routing for the bootstrap: the six built-in roles
(explore, research, plan, implement, review, verify) map to
`(provider, model)` entries resolved through layered configuration; every
resolution is persisted as a minimal routing decision record. No decision
model, no subscription awareness.

## Thin-slice scope

THIN SLICE: static table only, single provider (Neuralwatt GLM-5.2 per
spec §10.3). Role registry generalization, §9.19.2 precedence chains,
DecisionProvider, and subscription awareness deepen in E2.

## References

- `docs/spec/30-model-routing.md` §9.45 (role-based model routing — heuristic table first)
- `docs/spec/30-model-routing.md` §9.19.2 (per-domain and per-role routing precedence)
- `docs/spec/50-contracts-data.md` §9.22 (layered configuration)

## Acceptance criteria

- [ ] All six built-in roles resolve to a `(provider, model)` pair from config; `orc config get roles.implement.routing.provider` resolves
- [ ] Every resolution persists a decision record (role, provider, model, precedence path) to the local sqlite store
- [ ] A role with no table entry resolves via the documented fallback and records the fallback reason
- [ ] `cargo nextest run --workspace` green including routing-table unit tests

## QA scenarios

- Happy: resolve each of the six roles -> six decision records with distinct roles; evidence `.omo/evidence/<task-slug>/six-roles.txt`
- Failure: missing config layer -> typed configuration error naming the missing key (no default-to-first-model guess); evidence `.omo/evidence/<task-slug>/missing-key.txt`

## Non-goals

No DecisionProvider; no jev; no subscription/quota gating; no custom roles.

## Security impact

None (selection only; no credentials touched beyond existing provider config).

## Testing requirements

Unit tests over layered-config fixtures; determinism test (same inputs -> same resolution).

## Documentation impact

Config reference for `roles.<id>.routing.*`; CHANGELOG `[Unreleased]`.

## Dependencies

None.

## Rollback implications

Config-only; `orc config unset` removes the table.
""",
    },
    {
        "id": "E0-T4",
        "epic": "E0",
        "title": "Task: bootstrap-worker — headless mini-worker with the 4-tool minimal set",
        "risk": "Medium",
        "estimate": 8,
        "labels": [],
        "blocked_by": ["E0-T5"],
        "body": """## Objective

Headless one-shot mini-agent worker (mini-swe-agent pattern): task in,
structured result + PR out, exit code. Minimal mediated toolset of exactly
four tools: read files, search, mediated bash (via `arbitraitor_exec`), and
write via the transactional/normalization path. Attempt-bounded.

## Thin-slice scope

THIN SLICE: one-shot headless only; single worker shape; prompt minimal.
Standalone interactive mode, toolset hardening, and the MCP proxy deepen in
E4. Search via approved MCP servers lands in E0-T10.

## References

- `docs/spec/10-orchestrator.md` §9.38 (MCP-early tool strategy — worker toolset context)
- `docs/spec/60-milestones.md` §998 MVP-6 (built-in coding tools)
- `docs/spec/10-orchestrator.md` §9.33.3 (fresh-context implementer)
- Ledger F17/F21 (mini-swe-agent pattern: single loop, curated toolset, bounded)

## Acceptance criteria

- [ ] `orc worker run --task <id> --json` executes one leaf task end-to-end and exits 0 on success / non-zero on typed failure
- [ ] The worker has exactly four tools; any other capability request fails closed at the mediation boundary
- [ ] Every tool call crosses the Arbitraitor boundary and produces a receipt
- [ ] """ + BUDGET_BLOCK + """
- [ ] `cargo nextest run --workspace` green; worker loop unit tests use the deterministic simulator (no live provider in CI)

## QA scenarios

- Happy: worker completes a fixture leaf task against the simulator; structured result JSON contains task id, exit code, PR ref; evidence `.omo/evidence/<task-slug>/worker-happy.txt`
- Failure: task exceeding attempt budget stops with a typed exhaustion failure (no infinite retry); a tool call outside the 4-tool set is refused and recorded; evidence `.omo/evidence/<task-slug>/worker-bounds.txt`

## Non-goals

No interactive mode; no sub-agent spawning; no model self-selection (routing is E0-T3); no review behavior.

## Security impact

The worker is the untrusted principal: all I/O mediated; untrusted output quarantined per §9.14.

## Testing requirements

Simulator-backed unit + integration tests; negative tests for tool-set escape and budget exhaustion.

## Documentation impact

`docs/cli/` for `orc worker run`; CHANGELOG `[Unreleased]`.

## Dependencies

- Blocked by E0-T5 (mediation — the worker runs mediated from its first run).

## Rollback implications

Worker is a new binary path; no existing behavior modified.
""",
    },
    {
        "id": "E0-T5",
        "epic": "E0",
        "title": "Task: bootstrap-mediation — Restricted-sandbox mediation + capability preflight for the mini-worker",
        "risk": "Critical",
        "estimate": 8,
        "labels": ["needs-human-review"],
        "blocked_by": [],
        "body": """## Objective

Wire the mini-worker's execution behind Arbitraitor: spawn under
`arbitraitor_sandbox::SandboxMode::Restricted`, run
`arbitraitor_sandbox::compute_effective_controls()` as a fail-closed
capability preflight before any worker starts, and route bash through
`arbitraitor_exec::ExecutionContextBuilder` (mediated exec). Missing
controls block the run (explicitly labelled non-secure mode is NOT available
for the bootstrap loop).

## Thin-slice scope

THIN SLICE: single-platform (Linux) Restricted mode + preflight + mediated
bash. Full mediation surface (leases, receipts, approval-required handling,
gap-filed workflow) deepens in E5.

## References

- `docs/spec/40-arbitraitor-integration.md` §9.6 (sandbox integration — `compute_effective_controls`, `configure_command`, `apply_sandbox`)
- `docs/spec/40-arbitraitor-integration.md` §6.7 (sole security authority; fail closed)
- `docs/spec/40-arbitraitor-integration.md` §16.2 (mandatory security feature workflow)
- `docs/spec/tech-stack.md` §2.2 (real Arbitraitor identifiers)

## Acceptance criteria

- [ ] Worker spawn calls `compute_effective_controls(SandboxMode::Restricted, platform)` and refuses to start when any required control is unavailable (fail closed, typed error naming the missing control)
- [ ] Mediated bash goes through `arbitraitor_exec::ExecutionContextBuilder` with an explicit ExecutionPolicy; no direct `std::process` spawn on the worker path
- [ ] Preflight result is recorded in the run state (controls matrix + verdict)
- [ ] `cargo nextest run --workspace` green; negative test asserts the forbidden effect did not occur (a denied exec leaves no filesystem/network side effect)

## QA scenarios

- Happy: on the reference Linux platform, preflight reports Required controls Available and the worker runs mediated; evidence `.omo/evidence/<task-slug>/preflight-ok.txt`
- Failure: simulated missing control (fixture) -> worker refuses to start with the missing control named; a network-denied exec leaves no connection (assert, not just error-observed); evidence `.omo/evidence/<task-slug>/preflight-fail-closed.txt`

## Non-goals

No macOS/Windows (ADR-0024 fail-closed there); no approval flows (E5); no receipts store (E5); no Disposable mode.

## Security impact

Direct wiring of the sandbox boundary: Risk=Critical, needs-human-review. Orchestraitor implements no security primitive — it consumes Arbitraitor's.

## Testing requirements

Adversarial negatives (§21.4): assert forbidden effects did not happen; capability-matrix fixture tests.

## Documentation impact

`orc doctor` surfaces the preflight matrix; CHANGELOG `[Unreleased]`.

## Dependencies

None upstream in E0 (E0-T4 consumes this).

## Rollback implications

Fail-closed by construction; disabling mediation is not an option on this path.
""",
    },
    {
        "id": "E0-T6",
        "epic": "E0",
        "title": "Task: bootstrap-delivery — worktree→DCO commit→scoped push→draft PR",
        "risk": "Medium",
        "estimate": 5,
        "labels": [],
        "blocked_by": [],
        "body": """## Objective

Delivery path for worker output: provision a worktree per task, commit with
DCO sign-off (bot attribution per D11), push via a scoped lease credential,
and open a draft PR whose body carries the spec anchors and evidence links.

## Thin-slice scope

THIN SLICE: single-repo delivery, one branch naming scheme, draft PR only.
Full lifecycle (risk re-label, lease credential rotation, multi-repo) deepens
in E6.

## References

- `docs/spec/40-arbitraitor-integration.md` §6.2 (a worktree is not a sandbox — trusted controller owns Git metadata)
- `docs/spec/20-harness-worker.md` §9.4 (workspace and Git controller)
- `docs/spec/10-orchestrator.md` §9.33.2 (task fields, spec traceability)
- AGENTS.md (worktree-first rule; DCO; conventional commits)

## Acceptance criteria

- [ ] `orc worker run` output lands on branch `type/<slug>` in a per-task worktree; the main checkout is never touched
- [ ] Commits carry `Signed-off-by` (DCO) and the bot identity; repo ruleset verification passes
- [ ] Push uses a scoped, short-lived credential (installation token via E0-T1's minting path), never the owner's ambient auth
- [ ] Draft PR opened with spec anchors + evidence paths in the body; PR is a draft (never opened ready-to-merge)
- [ ] `cargo nextest run --workspace` green including lifecycle unit tests

## QA scenarios

- Happy: fixture task -> worktree -> commit -> push -> draft PR URL captured in the structured result; evidence `.omo/evidence/<task-slug>/delivery-happy.txt`
- Failure: push credential expired -> typed failure, no fallback to ambient auth, worktree preserved for retry; evidence `.omo/evidence/<task-slug>/push-expired.txt`

## Non-goals

No merge behavior (human-gated, always); no PR-time risk re-label (E6); no multi-repo.

## Security impact

Scoped credentials only; Git metadata owned by the trusted controller (worker never sees `.git` of the main checkout).

## Testing requirements

Integration test against a local bare remote fixture; negative test for credential expiry.

## Documentation impact

`docs/cli/` delivery flags; CHANGELOG `[Unreleased]`.

## Dependencies

- Consumes E0-T1 (bot identity + scoped credential) when available; degrades to labelled owner-auth fallback until then.

## Rollback implications

Worktrees are disposable; branches deletable; draft PRs closable — full revert path.
""",
    },
]
