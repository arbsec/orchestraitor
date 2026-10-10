---
name: github-pr-lifecycle
description: Drive a pull request from draft to safe merge through the Orchestraitor adversarial-review loop — create draft PRs, link issues, inspect CI and GitHub Actions failures, select fresh-context reviewers, fetch and deduplicate review-thread findings, run remediation loops, check documentation impact, verify convergence against the current HEAD, and reconcile state post-merge. Use whenever creating, reviewing, remediation-looping, gating, or merging a PR on a spec-driven GitHub project.
license: MIT OR Apache-2.0
compatibility: Requires gh CLI v2.94.0+ for sub-issue/dependency linking, jq for JSON parsing, and a GraphQL-capable gh auth (review threads are GraphQL-only — gh pr view --json reviewThreads does NOT exist). Scripts read review/merge policy from .agents/project/github-project.local.toml (example: github-project.example.toml); never hardcoded.
---

# github-pr-lifecycle

Owns the **PR half** of spec-driven delivery: draft → CI → review → remediate → converge → merge → reconcile. The sibling skill `github-project-workflow` owns the issue half. This skill is **generic mechanism**; project-specific policy (required reviewer domains, review-loop limits, PR convergence rules, forbidden administrative shortcuts) lives in `.agents/project/orchestraitor-workflow.md` and the project config file.

## When to use

- Creating a draft PR linked to a leaf issue + spec references.
- Inspecting CI/Actions failures and classifying them (transient vs. real failure vs. flake).
- Running an adversarial-review generation in a fresh context (spec `10-orchestrator.md` §9.33.3).
- Fetching review-thread state to deduplicate findings across loops.
- Verifying the PR checklist (the `<!-- orc:* -->` markers in `.github/PULL_REQUEST_TEMPLATE.md`).
- Deciding merge eligibility: all checks pass + threads resolved + convergence achieved.
- Performing a safe squash merge and post-merge reconciliation.

**Do NOT use this skill for**: issue triage, decomposition, ready-queue selection, or blocker edges — those belong to `github-project-workflow`. The two skills compose: this skill consumes issues that skill produces.

## What is in this skill

- **Core procedure** (below) — the always-run PR loop, kept under ~150 lines.
- **References** (loaded on demand):
  - [`references/pr-convergence.md`](references/pr-convergence.md) — the convergence rule: fresh-context reviews, HEAD invalidation, max-loop → `blocked`/`needs-human` (never silent approval).
  - [`references/review-findings.md`](references/review-findings.md) — severity taxonomy (CRITICAL/HIGH/MEDIUM/LOW), deduplication, tracking across loops.
  - [`references/gh-capabilities.md`](references/gh-capabilities.md) — verified `gh` CLI surface: `pr checks --json`, `pr view --json` (and its **missing** `reviewThreads`), `pr merge --squash --match-head-commit`, `pr review`.
  - [`references/graphql.md`](references/graphql.md) — the `pullRequest.reviewThreads` connection with `isResolved`/`isOutdated`/`comments`; resolve/unresolve mutations.
  - [`references/documentation-gate.md`](references/documentation-gate.md) — what counts as "public behavior" requiring same-PR doc updates (spec `10-orchestrator.md` §9.33).
  - [`references/verified-commit-path.md`](references/verified-commit-path.md) — why unsigned local bot-identity commits never verify, and the App-API replay path for unsigned chains (documented procedure; replay script not yet implemented).
  - [`references/review-thread-reply.md`](references/review-thread-reply.md) — the thread-reply contract: answers to inline review comments are posted as REPLIES on that comment's thread (REST replies endpoint / `addPullRequestReviewThreadReply`), never as new main-thread comments (owner directive 2026-10-07).
  - [`references/commit-hygiene-remediation.md`](references/commit-hygiene-remediation.md) — failure-class → detection → remediation map for commit-hygiene violations (identity, verification, message format, mode/permission and ghost-commit anomalies); what `audit-pr-commits` flags and how to fix it.
- **Scripts** (deterministic operations, in `scripts/`): each has `--help`, stable exit codes, `--json` output; mutating scripts support `--dry-run`.
  - `scripts/pr-review-local` — per-commit LOCAL CodeRabbit feedback (`coderabbit review --agent --base <PR base branch>`; read-only, never posts to GitHub). Exit codes: `0` review completed (findings or clean), `1` GitHub lookup/read failure, `2` config/state error (no PR for branch, jq missing for `--json`), `3` coderabbit CLI failure (missing binary included), `4` local HEAD or worktree mismatch, `5` mergeable-gate refusal (PR conflicts with base). Invoke as `bash .agents/skills/github-pr-lifecycle/scripts/pr-review-local ...` — skill scripts are intentionally non-executable (mode `100644`) because the App-signed landing path (`createCommitOnBranch` fileChanges) cannot create mode `100755`.

## Core procedure

```text
1. DRAFT          Create the PR as DRAFT linked to the leaf issue (Closes #N) and spec
                  references with `pr-create` (routes `gh pr create` through
                  `orc_lib_gh_service`; never call `gh pr create` directly).
                  The managed checklist (<!-- orc:* --> markers) starts
                  unchecked — checkboxes are verified facts, not intentions.
                  Then apply attribution labels with `pr-labels <pr>`:
                  `agent-created` (service-identity author) and
                  `needs-human-review` (diff touches a §21.1
                  security-sensitive class by path/content). Labels derive
                  from launch facts only, applied idempotently.
                  Human attention rides the labels:
                    - `needs-human-review` ⇒ request the required-domain
                      reviewer — `pr-request-review` once ALL automatic
                      checks are green; `pr-mutate edit <pr>
                      --add-reviewer` is the same gated fallback (it
                      refuses while any check is failing or pending, like
                      `pr-request-review`). Review SIGN-OFF is always a
                      reviewer, never an assignee.
                    - Assignee (`pr-mutate edit <pr> --add-assignee`) only
                      when a decision or ownership HANDOFF is needed
                      (assignees accept user accounts only; board ownership
                      rides Status per workflow policy). Never reviewer +
                      assignee by default
                      — route by what the label asks FOR (workflow policy
                      "PR labeling and human attention").

2. CI             Land commits with `orc github push-branch` (the App-signed
                  landing path — NEVER plain `git push`, which produces
                  unverified commits the `required_signatures` ruleset
                  rejects at merge time); watch checks with `pr-checks`.
                  Before any local `git commit` destined for a PR branch, run
                  `bash .agents/skills/github-pr-lifecycle/scripts/commit-identity
                  check` (or export the identity via
                  `eval "$(bash .agents/skills/github-pr-lifecycle/scripts/commit-identity
                  env)"`) — an ambient human identity
                  is a policy violation (audit class unauthorized-identity).
                  Audit commit hygiene with `audit-pr-commits <pr>` alongside
                  `pr-checks` — every PR commit must be authored by a declared
                  service principal, GitHub-verified, and Conventional Commits.
                  Violations are REAL failures: fix the root cause per
                  `references/commit-hygiene-remediation.md` (recreate the
                  offending commit(s) via the verified-commit path); never
                  reroll and never force-push an unsigned chain.
                  Classify each failure:
                    transient (timeout, 429, 5xx, runner OOM)  → bounded retry per spec `10-orchestrator.md` §9.26.2
                    real failure                              → fix root cause; do NOT reroll
                    flake                                     → identify + file issue; do NOT
                                                              rerun until green (spec `50-contracts-data.md` §21.10)
                  Required AND non-optional checks must ALL pass. A missing/skipped
                  check is a failure, not a pass — except workflow jobs deliberately
                  disabled repo-side (the `KNOWN_DISABLED_JOBS` allowlist in
                  `pr-checks`), which are not-applicable and excluded from the gate.

3. REVIEW         Select reviewers by changed area (spec `10-orchestrator.md` §9.33.4):
                    general, security, backend, frontend, data, devops, testing,
                    documentation — per .agents/project/orchestraitor-workflow.md.
                  Each reviewer MUST use a FRESH context (new agent spawn, not the
                  implementer's session). Implementers may not approve their own
                  security-sensitive changes (spec `50-contracts-data.md` §21.1).
                  This skill does NOT perform the review itself; it tracks generations.
                  Every generation uses the canonical prompt + report shape in
                  references/review-message-template.md (parameters, fixed report
                  sections, tone rules). An adversarial review by a FRESH agent
                  session (new spawn, zero implementer context; implementers may
                  not review their own changes — spec §21.1, §9.33.3) is REQUIRED
                  for every PR: `pr-review-agent <pr>` emits the review contract
                  + scope, and `pr-review-agent <pr> --record <file>` posts
                  in-thread findings and stores the verdict record
                  (references/agent-review.md). Convergence still requires the
                  recorded generation to be CLEAN against the current head.

4. FINDINGS       Fetch review threads with `review-threads` (GraphQL; `gh pr view
                  --json reviewThreads` DOES NOT EXIST — see gh-capabilities.md).
                  Each finding carries severity + evidence + path + rule + remediation.
                  Deduplicate across loops — see review-findings.md.
                  Fix all CRITICAL/HIGH; MEDIUM unless explicitly justified+recorded;
                  LOW may be deferred with recorded reasoning.
                  `@coderabbitai review` comments are never posted (owner
                  directive: the review mechanism is the LOCAL CLI, step 6).
                  Bot-generated review comments that already exist on the PR
                  (coderabbitai and similar) enter
                  the same findings pipeline — deduplicated and severity-verified
                  against code before remediation; never applied verbatim.

5. REMEDIATE       Apply fixes in a FRESH context (not the implementer's). Land
                   them with `orc github push-branch` (never plain `git push`).
                   Replies to inline review comments go through `pr-thread-reply`
                   on the comment's thread — never as new main-thread comments
                   (owner directive 2026-10-07; see references/review-thread-reply.md).
                   Each new commit INVALIDATES earlier review convergence — the next
                   review generation targets the CURRENT HEAD, not the prior diff.
                   This loop is AUTOMATIC: every review generation's findings are
                   remediated without operator involvement (spec §9.24 autonomous
                   review-fix loop) until convergence or a budget stop.
                   NAMING: each remediation landing carries its OWN
                   conventional-commit message naming the finding(s) it
                   addresses — `fix(review): <finding summary> (gen N)`.
                   NEVER reuse the PR's feature headline for a remediation
                   landing: identical duplicate commit titles destroy the
                   history's meaning and make review of the fix impossible.
                   Per-commit review feedback runs LOCALLY via
                   `pr-review-local` (wraps `coderabbit review --agent --base
                   <base>`; read-only, never posts to GitHub). Do NOT post
                   `@coderabbitai review` comments — the review mechanism is
                   the local CLI; iterate locally to zero actionable findings.

5b. HUMAN GATE    For PRs carrying `needs-human-review` (§21.1
                  security-sensitive classes): after checks turn GREEN and
                  review threads are resolved, request the human reviewer
                  with `pr-request-review <pr>` — the script refuses
                  (typed reasons) while ANY automatic check is failing or
                  pending: never ask for human attention before the machine
                  finished its own verification. The script ALSO refuses
                  while the PR conflicts with its base
                  (mergeable=CONFLICTING / mergeStateStatus=DIRTY): no review
                  of any kind — a human reviewer request OR an automated
                  review trigger (`@coderabbitai review` via pr-comment,
                  any pr-review-post verdict) — may be requested while the
                  PR is unmergeable; resolve conflicts with base first.
                  Idempotent; the reviewer is
                  the repo owner unless `--reviewer` overrides. The PR then
                  AWAITS the human's approval — the orchestrator MUST NOT
                  merge it autonomously (workflow policy; AGENTS.md).

6. CONVERGE       Stop when ONE full review generation against the current HEAD finds
                  NO new noteworthy findings AND all earlier blocking findings are
                  resolved. See pr-convergence.md.
                  The review mechanism is the LOCAL CodeRabbit CLI via
                  `bash .agents/skills/github-pr-lifecycle/scripts/pr-review-local`
                  (wraps `coderabbit review --agent --base <base>`; read-only,
                  never posts to GitHub). Iterate locally (remediate → rerun) to
                  ZERO actionable findings at the final HEAD. Do NOT post
                  `@coderabbitai review` comments. Convergence evidence = a local
                  CLI review generation with zero actionable findings at the final
                  HEAD, recorded by the session — cite the run + SHA in the PR
                  description.
                  Reaching a configured loop/cost/time limit produces a `blocked` or
                  `needs-human` state (spec `10-orchestrator.md` §9.24, §9.33.4) — NEVER silent approval.
                  Use `convergence-status` to compute the verdict from checks + threads
                  + checklist.

7. DOCS           Run `classify-docs-impact` against the diff. If any public-behavior
                  surface is touched (CLI, config, env vars, public APIs, daemon
                  protocol, built-in tools, MCP, provider support, security guarantees,
                  error behavior, install/migrate/remove), human-facing docs MUST
                  update in this same PR (spec `10-orchestrator.md` §9.33). CHANGELOG [Unreleased]
                  gains an entry per public-behavior change. Reconcile the checklist
                  with `reconcile-checklist`.

8. MERGE          Only when `merge-gate` exits 0:
                    - all required + non-optional checks pass (current HEAD)
                    - all actionable review threads resolved
                    - all noteworthy findings fixed or formally resolved (recorded)
                    - convergence achieved against current HEAD
                    - documentation updated
                    - PR checklist items checked based on EVIDENCE
                  `gh pr merge --squash --delete-branch --match-head-commit <sha>`
                  (executed as the App service identity via
                  `orc github gh-env --` when `github_app` config is present;
                  see Safety conditions).
                  Never use --admin to bypass a red gate (spec `50-contracts-data.md` §21.10, AGENTS.md).

9. RECONCILE      After merge: re-run `pr-labels <pr>` (labels can drift as the
                  diff changed during the review loop — the audit is cheap and
                  idempotent; it never removes labels). Then close the linked
                  issue (or confirm the PR's "Closes #N" did), delete the
                  branch, remove the worktree. Move the issue to
                  "Done" on the project (github-project-workflow skill owns that).
```

**Worktrees.** Lefthook hooks are shims in the SHARED `.git/hooks` directory:
`git worktree add` alone does NOT install them — a fresh worktree inherits
hooks only if `lefthook install` has run in the repo at least once (it binds
the hooks next to the common `.git`, which all worktrees then use). After
creating a worktree in a checkout where hooks were never installed, run
`lefthook install` once.

## Inputs

- **PR identifier**: number, URL, branch, or JSON from stdin (for piping).
- **Project config**: `.agents/project/github-project.local.toml` for review-loop limits, required reviewer domains, merge strategy. If absent, scripts exit `2`.
- **`gh` identity**: requires GraphQL access. Agent-driven operations authenticate as the GitHub App service identity via `orc github gh-env --` (see Safety conditions), not a personal account — ambient `gh auth status` is NOT a meaningful check (an installation token never appears there); verify the service route instead (`orc config get github_app.client_id` resolves; missing App permissions surface as typed failures from the wrapper). `orc` must be on the worker `PATH` (or point `ORC_BIN` at the binary, e.g. the repo's `target/release/orc` from `cargo build --release -p orchestraitor-cli`); without it, mutating operations fail closed when enforcement is `required`.

## Outputs

- Human-readable by default; `--json` for machine consumption and for piping between scripts.
- `merge-gate` prints a JSON verdict (`mergeable: bool`, `reasons: [...]`) and exits `0` only when mergeable.

## Failure states (stable exit codes)

| Code | Meaning |
|---|---|
| `0` | Success (PR is mergeable / `--dry-run` preview rendered / operation completed) |
| `1` | Unrecoverable error (network, auth, unexpected `gh` output, GraphQL schema mismatch) |
| `2` | Config/state error (missing project config, PR not found, head SHA mismatch) |
| `3` | Policy violation (would merge on red, would skip adversarial review, would use `--admin`) |
| `4` | Concurrent edit detected (PR's `updatedAt`/head SHA changed since read — re-read and retry) |
| `5` | Convergence not reached (review loop limit hit — produces `blocked`/`needs-human`, never silent approval) |

## Safety conditions (non-negotiable)

- **No merge on red.** `merge-gate` exits non-zero if any required or non-optional check is not passing. A skipped/missing check is a failure.
- **No admin bypass.** This skill never passes `--admin` to `gh pr merge`. Reaching a limit is a `blocked` state, not a merge path.
- **Fresh-context reviews only.** The skill tracks review *generations*; it does not let a reviewer approve their own implementation (spec `50-contracts-data.md` §21.1, `10-orchestrator.md` §9.33.3). Security-sensitive changes require human review before release.
- **HEAD is authoritative.** Reviews rerun against the current HEAD after each commit. Stale convergence is not convergence.
- **Verified commits / service-identity push path.** Agent branch pushes MUST land as GitHub-signed commits via `orc github push-branch` (GraphQL `createCommitOnBranch` + `updateRefs` with an exact-head precondition — never overwrites a concurrent commit; tree-equality and `verified = true` gates, fail-closed) — never as plain `git push` and never as locally-created unsigned bot-identity commits (the `required_signatures` ruleset blocks them; locally-attributed commits fail closed). The subcommand lands the local branch's tree as ONE squashed App-signed commit and creates or fast-forwards the remote branch by itself; no manual GraphQL recipe is needed. Before any local `git commit` destined for a PR branch, `bash .agents/skills/github-pr-lifecycle/scripts/commit-identity check` must pass — it also fails (exit 3, `local-signing-not-verified-path`) when `commit.gpgsign=true` in the checkout, because a locally-gpg-signed bot commit can never be GitHub-verified. For existing unsigned chains, replay per `references/verified-commit-path.md` (the replay script `scripts/github-app-recreate-branch.sh` is designated but not yet implemented — follow the documented steps). Pitfall: never reset a PR branch to an ancestor of main mid-replay — that auto-closes the PR; replay on a temp branch and swing the ref once.
- **Review threads via GraphQL.** `gh pr view` has no `reviewThreads` field (verified, see `gh-capabilities.md`). Use `gh api graphql` with the `pullRequest.reviewThreads` connection.
- **Dry-run first.** `merge-gate --dry-run` prints the verdict and the exact `gh pr merge` command it would run; writes nothing.
- **Match-head-commit.** `gh pr merge --squash` uses `--match-head-commit` to refuse merge if the head moved between the gate check and the merge call.
- **Service identity, not personal accounts.** Agent-driven PR, issue, and review operations MUST authenticate as the project's GitHub App service identity — never a personal account. PRs, comments, and reviews attribute to the bot identity; commits are authored with the bot identity and DCO sign-off. For Orchestraitor that is the `arbsec-agent` App (org-owned, installation-scoped; planning runbook `.omo/drafts/github-app-setup.md`). Mechanically: ALL mutating `gh` calls in these scripts route through `orc_lib_gh_service` (in `_lib.sh`), which wraps `gh` in `orc github gh-env --` when the `github_app` config resolves — the minted installation token is injected into the child's `GH_TOKEN` and never printed, logged, or persisted. PR creation, PR comments, and review posts go through the dedicated wrappers `pr-create`, `pr-comment`, and `pr-review-post` (same route; never call `gh pr create` / `gh pr comment` / `gh pr review` directly). On this repo the personal fallback is FORBIDDEN (owner mandate, 2026-10-02; `.agents/project/orchestraitor-workflow.md` "GitHub service identity"): if the service path fails — missing or partial `github_app` config, minting failure, `orc` unavailable — the operation FAILS with its typed error and MUST be reported to the orchestrator; never fall back to personal auth, never invoke `gh` directly for a mutating call. The `pr-review-post` wrapper never supports `--approve`: its service identity authors the PRs it reviews, and approvals require an independent authorized reviewer (GitHub forbids self-approval), so the verdict is refused with a typed error; `--comment` and `--request-changes` are supported. Enforcement is pinned to `required` (`github_app.enforcement` in `orchestraitor.toml`): a missing or unresolvable App config yields a typed config error (exit 2), no fallback, no WARNING. Wrapper-only deployments may pin `ORC_GITHUB_APP_ENFORCEMENT=required`, which wins over the layered config and fails closed even when `orc` is unavailable. When `orc` is unavailable and no env pin is set, the wrapper reads the pinned declaration directly from the repo `orchestraitor.toml` (`$(git rev-parse --show-toplevel)/orchestraitor.toml`, overridable via `ORC_REPO_TOML`): a repo that pins `required` fails closed even without the tooling. Caller identity for assignee-ownership checks resolves via `orc_lib_resolve_my_login` (in `_lib.sh`), scoped strictly to the selected route: when `orc` is present (service route) the principal is the App bot login from `orc github commit-author` — if that fails, the result is EMPTY, never the personal login (a personal login on the service route would be a principal mismatch in the ownership checks); `gh api user` is used only when the ambient route is selected. An empty resolution is a typed failure, never a wildcard match — the calling scripts exit with the config error code when identity cannot be resolved.

## How this skill relates to project policy

This skill describes **how** to drive a PR generically. **Whether** the Orchestraitor rules permit merging is policy — see `.agents/project/orchestraitor-workflow.md` (MVP-only scheduling, required reviewer domains, forbidden administrative shortcuts). If the two disagree, project policy wins; update this skill.

## Keeping the skill current

- Re-verify `gh` flags against [`references/gh-capabilities.md`](references/gh-capabilities.md) on `gh` bumps. The `reviewThreads` GraphQL connection and `gh pr checks --json` field shapes have shifted before.
- Update [`references/graphql.md`](references/graphql.md) if GitHub renames review-thread mutations (`resolveReviewThread` etc.).
- Keep this file **under 500 lines** (Agent Skills spec). Move new detail to a reference, not here.
