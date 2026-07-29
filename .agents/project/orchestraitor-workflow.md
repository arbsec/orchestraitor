# Orchestraitor workflow policy

Project-specific delivery rules for the Orchestraitor repository. The reusable
[github-project-workflow](../skills/github-project-workflow/SKILL.md) and
[github-pr-lifecycle](../skills/github-pr-lifecycle/SKILL.md) skills hold the generic
mechanism; this file holds Orchestraitor's choices.

This file is **project-specific**. Do not leak Orchestraitor's concrete values into the
generic skill instructions. The skills read mutable configuration (organization, project
number, field names) from [`github-project.example.toml`](github-project.example.toml) or the
local equivalent, never from hardcoded values.

## MVP-only scheduling

During the MVP phase, only issues that satisfy **all** of the following may be implemented:

- `Type` is `Task` or `Bug` (Epics and Features MUST be decomposed into leaf issues first).
- `Target` = `MVP` (references a requirement in [`docs/spec/spec.md`](../../docs/spec/spec.md)
  §998 MVP requirements or a §9 subsystem the MVP depends on).
- `Status` = `Ready` (Definition of Ready met — see
  [`../skills/github-project-workflow/references/definitions-of-ready-and-done.md`](../skills/github-project-workflow/references/definitions-of-ready-and-done.md)).
- No unresolved `Blocked by` dependencies (native issue dependencies).
- No conflicting in-flight PR touching the same spec section, crate, or invariant
  (`AGENTS.md`, spec §9.33.2).

Post-MVP items (`Target = Post-MVP`, spec §999) are never scheduled during the MVP phase, even
if they look easy. Open them; do not implement them.

## Security-first review

- **Orchestraitor implements no security primitive** (spec §2.2, §16). A Task that needs new
  security behavior is `Blocked` on an `arbsec/arbitraitor` issue/PR; the Orchestraitor Task
  links it and does not start until the Arbitraitor capability lands.
- Changes to the Arbitraitor integration boundary (`crates/orchestraitor-arbitraitor-client/`),
  provider transports/proxy (secrets, untrusted protocol input), capability issuance, output
  promotion, network/secret handling, or `unsafe` require **human review before release**
  (spec §21.1) and are routed to `@arbsec/security` via
  [`CODEOWNERS`](../../.github/CODEOWNERS).
- Adversarial review continues until one full review generation against the current HEAD finds
  no new noteworthy findings and all earlier blocking findings are resolved (PR convergence;
  see the pr-lifecycle skill's `pr-convergence` reference). Reaching a loop/cost/time limit
  produces `blocked` / `needs-human` — never silent approval.

## Arbitraitor ownership boundaries

When the project-manager agent selects a Task, it checks ownership before scheduling:

| Concern | Owner | Action if missing there |
|---|---|---|
| Sandboxing, effective-control probes | Arbitraitor (`arbitraitor-sandbox`) | Open Arbitraitor issue; Orchestraitor Task stays `Blocked` |
| Policy evaluation + traces | Arbitraitor (`arbitraitor-policy`) | Open Arbitraitor issue; Orchestraitor Task stays `Blocked` |
| Approvals, `ApprovalTokenIssuer`, `PlanContext` | Arbitraitor (`arbitraitor-mcp`) | Open Arbitraitor issue; Orchestraitor Task stays `Blocked` |
| Receipts, signing, in-toto export | Arbitraitor (`arbitraitor-receipt`) | Open Arbitraitor issue; Orchestraitor Task stays `Blocked` |
| Workspace projection / VFS / overlay / promotion authorization | Arbitraitor | Open Arbitraitor issue; Orchestraitor uses `materialized` backend + reports the gap (spec §9.4.2, §16.8) |
| Agent loop, adapters, context compiler, format-on-write, TUI/CLI/IDE, presentation of decisions | Orchestraitor | Implement here |

Cross-repository blocker handling: an Orchestraitor issue blocked on Arbitraitor MUST link the
canonical Arbitraitor issue and carry the `blocked:arbitraitor` label. The ready-queue script
excludes any issue with an unresolved `blockedBy` edge, so the issue stays out of the queue
until the upstream PR lands. The project-manager does not retry such a Task as if it were
transiently blocked (spec §9.26.1); it waits on the upstream PR.

## Required review domains

Reviewer selection is based on changed areas (spec §9.33.4). For Orchestraitor the required
reviewer domains, when the corresponding area is touched, are:

| Touched area | Required reviewer domain |
|---|---|
| `crates/orchestraitor-arbitraitor-client/`, any approval/promotion/capability wiring | `security` (analysis) + human security-owner review |
| `crates/orchestraitor-provider-*` (secrets, untrusted protocol input, proxy) | `security` + `backend` |
| `crates/orchestraitor-workspace/`, `crates/orchestraitor-mcp/` (transaction/normalization, MCP gateway) | `backend` |
| `crates/orchestraitor-tui/`, CLI, IDE adapters | `frontend` / `documentation` |
| `docs/spec/**`, `AGENTS.md`, governance | `documentation` + maintainer |
| Tests, fixtures, conformance | `testing` |

The `security` domain is **analysis only** — it never implements enforcement (spec §9.19.1).

## Documentation expectations

Any change to public behavior updates human-facing docs in the same PR (`AGENTS.md`,
spec §9.17.1/§9.33). For Orchestraitor "public behavior" includes: `orc`/`orcd` commands and
flags, `orchestraitor.toml` schema, environment variables (`ORCHESTRATOR_*`,
`NEURALWATT_API_KEY`, `ZHIPU_API_KEY`), the daemon protocol, built-in tools, MCP/ACP behavior,
provider support, security guarantees, error behavior, and installation/migration/removal.
`CHANGELOG.md` `[Unreleased]` carries an entry per public-behavior change.

## Testing expectations

- Test Orchestraitor's behavior, not third-party internals (spec §21.2.3).
- Security-sensitive behavior gets negative + adversarial tests asserting the forbidden effect
  did **not** occur (spec §21.4).
- CI never depends on a live model provider — use the deterministic simulator
  (`orchestraitor-testkit`, spec §21.3).
- Every defect found during work gains a regression test when practical.

## Qlty and coverage expectations

Qlty (or an equivalent quality gate) and coverage reporting are wired up with the first
application code. Until then: no coverage target is enforced, but the intended target (once
`Cargo.toml` lands) is enforced coverage on the Arbitraitor integration boundary and the
transaction/normalization engine, with the parity gate in
[`docs/spec/tech-stack.md` §15](../../docs/spec/tech-stack.md) as the source of truth.

## PR convergence requirements

A PR merges only when (re-stating `AGENTS.md` in operational terms the pr-lifecycle skill
enforces):

1. all required and non-optional checks pass (current HEAD);
2. all actionable review threads are resolved;
3. all noteworthy findings are fixed or formally resolved with recorded reasoning;
4. one full adversarial-review generation against the current HEAD finds no new noteworthy
   findings;
5. required documentation is updated;
6. the managed PR-checklist items are checked based on evidence.

## Forbidden administrative shortcuts

- No merge on red. No admin-merge bypass.
- No re-running a flaky check until it passes by chance (spec §21.10).
- No implementer approving their own security-sensitive change (spec §21.1).
- No treating a loop/cost/time limit as successful convergence — it produces `blocked` /
   `needs-human` (spec §9.24, §9.33.4).
- No deferring a security/correctness defect needed for safe completion merely to shrink a PR
  (spec §9.33.2).
- No committing a `Cargo.toml` workspace without simultaneously adding the parity-gate
  workflows (see [`.github/workflows/README.md`](../../.github/workflows/README.md)).
