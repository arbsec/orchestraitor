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
- Running an adversarial-review generation in a fresh context (spec §9.33.3).
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
  - [`references/documentation-gate.md`](references/documentation-gate.md) — what counts as "public behavior" requiring same-PR doc updates (spec §9.17.1, §9.33).
- **Scripts** (deterministic operations, in `scripts/`): each has `--help`, stable exit codes, `--json` output; mutating scripts support `--dry-run`.

## Core procedure

```text
1. DRAFT          Create the PR as DRAFT linked to the leaf issue (Closes #N) and spec
                  references. The managed checklist (<!-- orc:* --> markers) starts
                  unchecked — checkboxes are verified facts, not intentions.

2. CI             Push commits; watch checks with `pr-checks`.
                  Classify each failure:
                    transient (timeout, 429, 5xx, runner OOM)  → bounded retry per spec §9.26.2
                    real failure                              → fix root cause; do NOT reroll
                    flake                                     → identify + file issue; do NOT
                                                              rerun until green (spec §21.10)
                  Required AND non-optional checks must ALL pass. A missing/skipped
                  check is a failure, not a pass.

3. REVIEW         Select reviewers by changed area (spec §9.33.4):
                    general, security, backend, frontend, data, devops, testing,
                    documentation — per .agents/project/orchestraitor-workflow.md.
                  Each reviewer MUST use a FRESH context (new agent spawn, not the
                  implementer's session). Implementers may not approve their own
                  security-sensitive changes (spec §21.1).
                  This skill does NOT perform the review itself; it tracks generations.

4. FINDINGS       Fetch review threads with `review-threads` (GraphQL; `gh pr view
                  --json reviewThreads` DOES NOT EXIST — see gh-capabilities.md).
                  Each finding carries severity + evidence + path + rule + remediation.
                  Deduplicate across loops — see review-findings.md.
                  Fix all CRITICAL/HIGH; MEDIUM unless explicitly justified+recorded;
                  LOW may be deferred with recorded reasoning.

5. REMEDIATE       Apply fixes in a FRESH context (not the implementer's). Push.
                  Each new commit INVALIDATES earlier review convergence — the next
                  review generation targets the CURRENT HEAD, not the prior diff.

6. CONVERGE       Stop when ONE full review generation against the current HEAD finds
                  NO new noteworthy findings AND all earlier blocking findings are
                  resolved. See pr-convergence.md.
                  Reaching a configured loop/cost/time limit produces a `blocked` or
                  `needs-human` state (spec §9.24, §9.33.4) — NEVER silent approval.
                  Use `convergence-status` to compute the verdict from checks + threads
                  + checklist.

7. DOCS           Run `classify-docs-impact` against the diff. If any public-behavior
                  surface is touched (CLI, config, env vars, public APIs, daemon
                  protocol, built-in tools, MCP, provider support, security guarantees,
                  error behavior, install/migrate/remove), human-facing docs MUST
                  update in this same PR (spec §9.17.1, §9.33). CHANGELOG [Unreleased]
                  gains an entry per public-behavior change. Reconcile the checklist
                  with `reconcile-checklist`.

8. MERGE          Only when `merge-gate` exits 0:
                    - all required + non-optional checks pass (current HEAD)
                    - all actionable review threads resolved
                    - all noteworthy findings fixed or formally resolved (recorded)
                    - convergence achieved against current HEAD
                    - documentation updated
                    - PR checklist items checked based on EVIDENCE
                  `gh pr merge --squash --delete-branch --match-head-commit <sha>`.
                  Never use --admin to bypass a red gate (spec §21.10, AGENTS.md).

9. RECONCILE      After merge: close the linked issue (or confirm the PR's "Closes #N"
                  did), delete the branch, remove the worktree. Move the issue to
                  "Done" on the project (github-project-workflow skill owns that).
```

## Inputs

- **PR identifier**: number, URL, branch, or JSON from stdin (for piping).
- **Project config**: `.agents/project/github-project.local.toml` for review-loop limits, required reviewer domains, merge strategy. If absent, scripts exit `2`.
- **`gh` auth**: requires GraphQL access (default `gh auth` scope is sufficient for read; mutations on review threads use the same scope).

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
- **Fresh-context reviews only.** The skill tracks review *generations*; it does not let a reviewer approve their own implementation (spec §21.1, §9.33.3). Security-sensitive changes require human review before release.
- **HEAD is authoritative.** Reviews rerun against the current HEAD after each commit. Stale convergence is not convergence.
- **Review threads via GraphQL.** `gh pr view` has no `reviewThreads` field (verified, see `gh-capabilities.md`). Use `gh api graphql` with the `pullRequest.reviewThreads` connection.
- **Dry-run first.** `merge-gate --dry-run` prints the verdict and the exact `gh pr merge` command it would run; writes nothing.
- **Match-head-commit.** `gh pr merge --squash` uses `--match-head-commit` to refuse merge if the head moved between the gate check and the merge call.

## How this skill relates to project policy

This skill describes **how** to drive a PR generically. **Whether** the Orchestraitor rules permit merging is policy — see `.agents/project/orchestraitor-workflow.md` (MVP-only scheduling, required reviewer domains, forbidden administrative shortcuts). If the two disagree, project policy wins; update this skill.

## Keeping the skill current

- Re-verify `gh` flags against [`references/gh-capabilities.md`](references/gh-capabilities.md) on `gh` bumps. The `reviewThreads` GraphQL connection and `gh pr checks --json` field shapes have shifted before.
- Update [`references/graphql.md`](references/graphql.md) if GitHub renames review-thread mutations (`resolveReviewThread` etc.).
- Keep this file **under 500 lines** (Agent Skills spec). Move new detail to a reference, not here.
