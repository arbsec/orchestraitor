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
- `Target` = `MVP` (references a requirement in
  [`docs/spec/60-milestones.md`](../../docs/spec/60-milestones.md)
  §998 MVP requirements or a §9 subsystem the MVP depends on).
- `Status` = `Ready` (Definition of Ready met — see
  [`../skills/github-project-workflow/references/definitions-of-ready-and-done.md`](../skills/github-project-workflow/references/definitions-of-ready-and-done.md)).
- No unresolved `Blocked by` dependencies (native issue dependencies).
- No conflicting in-flight PR touching the same spec section, crate, or invariant
  (`AGENTS.md`, spec `10-orchestrator.md` §9.33.2).

Post-MVP items (`Target = Post-MVP`, spec `60-milestones.md` §999) are never scheduled during the MVP phase, even
if they look easy. Open them; do not implement them.

## Parallel delivery lanes

The orchestrator runs independent leaf work in parallel lanes instead of serializing it behind
the one-claim guard:

- Only leaf Tasks/Bugs with **file-disjoint** scopes run in parallel. Lanes that touch the same
  spec section, crate, or invariant stay serialized, in which case the orchestrator keeps the
  default one-claim behavior.
- One worktree and one branch per lane (`git worktree add -b <type>/<slug>
  ../orchestraitor-<slug> origin/main`); lanes never share a checkout and never write to
  `main`. Claims are taken deliberately with `claim-issue --allow-parallel` (up to 4 concurrent
  claims), which keeps the one-claim guard active for every caller that omits the flag.
- Each lane rebases onto the updated `origin/main` before its PR opens, and merge order onto
  `main` is serialized by the orchestrator — never by whichever lane happens to finish first.
- Claims are reconciled through the project-workflow skill when a lane stalls or its PR merges:
  a merged or abandoned lane releases its claim (`release-issue`, then `reconcile` for board
  state) before another lane claims that issue.

## Replay base policy

Verified-commit replays on this repo target `main` (the default branch): the
PR's base ref, per the generic mechanism in
`.agents/skills/github-pr-lifecycle/references/verified-commit-path.md`.

## GitHub service identity

All agent-driven GitHub operations — board writes, issue lifecycle, PR
creation/comments, reviews, merges — run as the Orchestraitor GitHub App
service identity (`arbsec-agent`), never a personal account (`AGENTS.md`
critical rules; runbook `.omo/drafts/github-app-setup.md`). The App is
registered and installed (issue #307): installation tokens are minted from the
App private key by `orc github mint-token` / `orchestraitor-core`
`GitHubAppAuth` (config block `github_app` + `service_identities`; private key
via `secret://` URIs, fail-closed, ~1h tokens). There is NO personal-account
option: if the App identity is unavailable, the operation fails — it is never
rerouted to a personal account, labelled or otherwise.

**Owner mandate (2026-10-02): NO GitHub writes under the personal account
(`@mekwall`) — ever, on this repository.** There is no personal fallback for
this repo, labelled or otherwise:

- Every GitHub write (PR creation, PR comments, review posts, merges, issue
  and board writes) MUST go through the service wrapper — the skill scripts'
  `orc_lib_gh_service` route (`orc github gh-env --`) or a direct
  `orc github` subcommand. The pr-lifecycle scripts `pr-create`, `pr-comment`,
  and `pr-review-post` wrap `gh pr create`, `gh pr comment`, and
  `gh pr review` respectively.
- If the service path fails (missing/partial `github_app` config, minting
  failure, tool unavailable), the operation FAILS: report the typed error to
  the orchestrator and stop. NEVER fall back to personal auth, never retry
  the write outside the wrapper, never invoke `gh` directly for a mutating
  call.
- `github_app.enforcement` is pinned to `"required"` in
  [`orchestraitor.toml`](../../orchestraitor.toml): with the pin in place a
  missing or unresolvable App config produces a typed config error (exit 2),
  never the personal fallback. Wrapper-only deployments may additionally pin
  `ORC_GITHUB_APP_ENFORCEMENT=required` (it wins over the layered config).
- Workers MUST run from a FRESH checkout at the current `origin/main` (or
  rebase onto it before any GitHub write). A stale checkout that predates the
  service-identity wrappers silently bypasses them — verify the wrappers
  exist (`.agents/skills/*/scripts/_lib.sh` carries `orc_lib_gh_service`)
  before performing GitHub operations.
- `orc` MUST be on the worker `PATH` (`ORC_BIN` overrides the binary name in
  the skill scripts). There is no distributed binary yet: build it from the
  repo checkout with `cargo build --release -p orchestraitor-cli` and put
  `target/release/orc` on `PATH` (e.g. `export
  PATH="$repo/target/release:$PATH"` in the worker environment) or point
  `ORC_BIN` at it directly. If `orc` is unavailable, mutating GitHub
  operations fail closed under `required` enforcement — that is the correct
  outcome; fix the environment, do not route around it.

Assignee fields accept user accounts only, so the bot cannot carry ownership: board Status +
Orchestraitor run-state do. The ready queue excludes items assigned to humans and keeps
items assigned to a declared service identity schedulable
(spec `10-orchestrator.md` §9.41; the declared set is `service_identities` in layered config
and `[service_identities].slugs` in the project config, default `arbsec-agent`).

## Security-first review

- **Orchestraitor implements no security primitive** (spec `40-arbitraitor-integration.md` §2.2, §16). A Task that needs new
  security behavior is `Blocked` on an `arbsec/arbitraitor` issue/PR; the Orchestraitor Task
  links it and does not start until the Arbitraitor capability lands.
- Changes to the Arbitraitor integration boundary (`crates/orchestraitor-arb-client/`),
  provider transports/proxy (secrets, untrusted protocol input), capability issuance, output
  promotion, network/secret handling, or `unsafe` require **human review before release**
  (spec `50-contracts-data.md` §21.1) and are routed to `@arbsec/security` via
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
| Workspace projection / VFS / overlay / promotion authorization | Arbitraitor | Open Arbitraitor issue; Orchestraitor uses `materialized` backend + reports the gap (spec `20-harness-worker.md` §9.4.2, `40-arbitraitor-integration.md` §16.8) |
| Agent loop, adapters, context compiler, format-on-write, TUI/CLI/IDE, presentation of decisions | Orchestraitor | Implement here |

Cross-repository blocker handling: an Orchestraitor issue blocked on Arbitraitor MUST link the
canonical Arbitraitor issue and carry the `blocked:arbitraitor` label. The ready-queue script
excludes any issue with an unresolved `blockedBy` edge, so the issue stays out of the queue
until the upstream PR lands. The project-manager does not retry such a Task as if it were
transiently blocked (spec `10-orchestrator.md` §9.26.1); it waits on the upstream PR.

## Required review domains

Reviewer selection is based on changed areas (spec `10-orchestrator.md` §9.33.4). For Orchestraitor the required
reviewer domains, when the corresponding area is touched, are:

| Touched area | Required reviewer domain |
|---|---|
| `crates/orchestraitor-arb-client/`, any approval/promotion/capability wiring | `security` (analysis) + human security-owner review |
| `crates/orchestraitor-provider-*` (secrets, untrusted protocol input, proxy) | `security` + `backend` |
| `crates/orchestraitor-workspace/`, `crates/orchestraitor-mcp/` (transaction/normalization, MCP gateway) | `backend` |
| `crates/orchestraitor-tui/`, CLI, IDE adapters | `frontend` / `documentation` |
| `docs/spec/**`, `AGENTS.md`, governance | `documentation` + maintainer |
| Tests, fixtures, conformance | `testing` |

The `security` domain is **analysis only** — it never implements enforcement (spec `30-model-routing.md` §9.19.1).

## Documentation expectations

Any change to public behavior updates human-facing docs in the same PR (`AGENTS.md`,
spec `10-orchestrator.md` §9.33 and the documentation expectations in this
policy). For Orchestraitor "public behavior" includes: `orc`/`orcd` commands and
flags, `orchestraitor.toml` schema, environment variables (`ORCHESTRATOR_*`,
`NEURALWATT_API_KEY`, `ZHIPU_API_KEY`), the daemon protocol, built-in tools, MCP/ACP behavior,
provider support, security guarantees, error behavior, and installation/migration/removal.
`CHANGELOG.md` `[Unreleased]` carries an entry per public-behavior change and serves
consumers of Orchestraitor only — it is release notes, not a development log. Internal
development notes (spec-section bookkeeping, repository tooling, agent workflows, review
process, issue/task tracking) stay in PR descriptions, spec documents, and evidence files.

## Testing expectations

- Test Orchestraitor's behavior, not third-party internals (spec `50-contracts-data.md` §21.2.3).
- Security-sensitive behavior gets negative + adversarial tests asserting the forbidden effect
  did **not** occur (spec `50-contracts-data.md` §21.4).
- CI never depends on a live model provider — use the deterministic simulator
  (`orchestraitor-testkit`, spec `50-contracts-data.md` §21.3).
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

## Merge authority

- **Autonomous merge (owner directive, 2026-09-26, ratified in PR #405):** once all
  convergence requirements above hold **and the PR is not merge-blocked by the human-review
  exception below**, the orchestrator merges the PR itself as the `arbsec-agent` service
  identity. The ruleset (`required_signatures`, extension-approval for unattributed changes,
  CodeQL/coverage thresholds) remains the independent technical gate. Required reviewer
  domains — including maintainer review for governance changes (e.g. `docs/spec/**`,
  `AGENTS.md`, `.agents/**`, `.github/**` policy) — remain convergence preconditions; "no
  standing human gate" removes no domain requirement, only the separate human merge-approval
  step.
- **Human review exception:** changes in the spec `50-contracts-data.md` §21.1
  security-sensitive classes (privilege boundaries, sandboxing, policy, capability issuance,
  filesystem projection, network/secret handling, `unsafe`) — including any dependency
  update or refactor that touches those classes or the areas enumerated under
  **Security-first review** above — get the `needs-human-review` label (the GitHub projection
  of the `blocked` / `needs-human` escalation state, spec `10-orchestrator.md` §9.24,
  §9.33.4) and stay open for the owner. The orchestrator MUST NOT autonomously merge a PR
  carrying that label or touching those classes or areas; classification is by path and diff
  content, not by intent. Dependency updates that do not touch those classes or areas are
  reviewed by the agent security-reviewer domain and follow the autonomous path.
- Convergence that cannot be reached within the configured loop/cost/time budget produces
  `blocked` / `needs-human` state — never an automatic merge (spec `10-orchestrator.md`
  §9.24, §9.33.4).

## PR labeling and human attention

PRs carry two attribution labels, applied by the pr-lifecycle skill's
`pr-labels` script immediately after `pr-create` and re-checked at reconcile
(labels can drift as the diff changes). They derive **only from observable
facts** — the PR author and the changed paths/diff content — never from
titles, model-generated text, or PR descriptions:

| Label | Applied when |
|---|---|
| `agent-created` | the PR author is a declared service-identity principal (`app/arbsec-agent` / `arbsec-agent[bot]`, or any configured bot) |
| `needs-human-review` | the diff touches a spec `50-contracts-data.md` §21.1 security-sensitive class by path **or** added-line content |

Human attention is routed by what the label asks **for**, and never doubled
up by default:

| Need | Route |
|---|---|
| Review sign-off (a `needs-human-review` label) | a **reviewer** — `pr-request-review` once checks are green; `pr-mutate edit --add-reviewer` while checks still run |
| A decision or ownership handoff | an **assignee** (`pr-mutate edit --add-assignee` or `gh edit --add-assignee`) |

Assignee fields accept user accounts only, so the bot cannot carry ownership —
board ownership rides project Status (see "GitHub service identity" above).
Review sign-off is always a reviewer, never an assignee. Never request a
reviewer AND an assignee for the same need by default: pick the route the
label asks for.

## Commit hygiene

Every commit on a PR in this repository MUST be:

1. **Authored by a declared service principal** — `arbsec-agent[bot]` or
   `renovate[bot]`. A commit authored as the human owner (`mekwall`) riding
   an agent-created PR is identity misuse; a commit whose author email
   resolves to no GitHub account (`NO_USER`) is unattributable. Committer
   identity is GitHub's `web-flow` on App-created commits (expected, not a
   mismatch).
2. **GitHub-verified** — `commit.verification.verified = true`. Unsigned
   locally-created commits never verify (attribution metadata is not a
   signature; the `required_signatures` ruleset blocks them at merge).
3. **Conventional Commits** — subject matches the repo's type list
   (`feat`, `fix`, `security`, `docs`, `refactor`, `test`, `ci`, `chore`,
   `build`, `perf`, `revert`), the same shape `cog verify` enforces locally.

These properties are audited in the PR loop by the pr-lifecycle skill's
`audit-pr-commits` script (alongside `pr-checks` in step 2): local hooks
(`cog verify`, `check-unsigned-bot-commits`) never run on App-API commits, so
the audit is the only post-hoc enforcement. A violation is a real failure:
recreate the offending commit(s) via the verified-commit path
([`.agents/skills/github-pr-lifecycle/references/commit-hygiene-remediation.md`](../skills/github-pr-lifecycle/references/commit-hygiene-remediation.md)
— the remediation map for every failure class) — never reroll, never
force-push an unsigned chain, never reset a PR branch to an ancestor of
`main` mid-replay.

## Forbidden administrative shortcuts

- No merge on red. No admin-merge bypass.
- No re-running a flaky check until it passes by chance (spec `50-contracts-data.md` §21.10).
- No implementer approving their own security-sensitive change (spec `50-contracts-data.md` §21.1).
- No treating a loop/cost/time limit as successful convergence — it produces `blocked` /
   `needs-human` (spec `10-orchestrator.md` §9.24, §9.33.4).
- No deferring a security/correctness defect needed for safe completion merely to shrink a PR
  (spec `10-orchestrator.md` §9.33.2).
- No committing a `Cargo.toml` workspace without simultaneously adding the parity-gate
  workflows (see [`.github/workflows/README.md`](../../.github/workflows/README.md)).

## Housekeeping

- After a PR merges (or is closed/handed off): delete the branch (local + remote if
  present) and remove the task worktree — `git worktree remove <path>` +
  `git worktree prune`; never leave merged worktrees on disk.
- Reclaim build artifacts regularly: `cargo clean` in the main checkout and any
  long-lived worktree; stale `target/` directories are pure build cache and must not
  accumulate across worktrees.
- Before removing a worktree, verify it is clean (`git status --porcelain` empty) and its
  work is landed (merged PR or explicit handoff); a dirty or unmerged worktree is kept
  and reported, never force-removed.
