---
name: github-project-workflow
description: Manage GitHub issues through the Orchestraitor delivery workflow — triage issue types and project fields, select work from the MVP ready-queue, decompose Epics/Features into leaf sub-issues, track blockers via native issue dependencies, claim and release issues, and open cross-repository Arbitraitor prerequisites. Use whenever selecting, decomposing, claiming, blocking, or reconciling issue state on a spec-driven GitHub project.
license: MIT OR Apache-2.0
compatibility: Requires gh CLI v2.94.0+ (issue types, sub-issues, dependencies), jq for JSON parsing, and a GitHub Project (V2) with the project's configured fields. Scripts read field/option names from .agents/project/github-project.local.toml (example: github-project.example.toml) and resolve node IDs at runtime — never hardcoded.
---

# github-project-workflow

Manages the **issue lifecycle** half of spec-driven delivery: triage → decomposition → ready-queue → claim → block/unblock → reconcile. The sibling skill `github-pr-lifecycle` owns the PR half. This skill is **generic mechanism**; project-specific policy (MVP-only scheduling, required reviewer domains, Arbitraitor ownership boundaries) lives in `.agents/project/orchestraitor-workflow.md` and the project config file, never in these instructions.

## When to use

- An issue was opened or updated and needs triage into the project's Type/Target/Status/Domain.
- The project-manager role needs the next eligible leaf Task/Bug from the ready-queue.
- An Epic/Feature must be decomposed into native sub-issues before any leaf may start.
- Work is blocked by another issue — same repo or cross-repo (Arbitraitor prerequisite).
- An agent is claiming, abandoning, or handing off an issue.
- Newly discovered work needs a separate issue (independently testable/revertible follow-up).

**Do NOT use this skill for**: PR review, CI inspection, merge decisions — those belong to `github-pr-lifecycle`. The two skills are intentionally separate so the issue model and the PR model can evolve independently.

## What is in this skill

- **Core procedure** (below) — the always-run loop, kept under ~150 lines.
- **References** (loaded on demand):
  - [`references/issue-model.md`](references/issue-model.md) — Type/Target/Status/Domain/Risk/Autonomy fields, leaf-vs-decomposable rule.
  - [`references/definitions-of-ready-and-done.md`](references/definitions-of-ready-and-done.md) — Definition of Ready (schedulable) and Definition of Done (mergeable).
  - [`references/task-decomposition.md`](references/task-decomposition.md) — when and how to decompose into native sub-issues.
  - [`references/discovered-work.md`](references/discovered-work.md) — blocker vs. follow-up vs. hidden-in-PR; spec §9.33.2.
  - [`references/gh-capabilities.md`](references/gh-capabilities.md) — verified `gh` CLI surface (v2.94.0+): issue types, sub-issues, dependencies, `--json` fields.
  - [`references/graphql.md`](references/graphql.md) — GraphQL for sub-issues, blocked-by, project fields/options; when `gh` first-class commands are insufficient.
- **Scripts** (deterministic operations, in `scripts/`): each has `--help`, stable exit codes, `--json` output, and `--dry-run` on remote-mutating operations.

## Core procedure

```text
1. TRIAGE       Confirm the issue has Type/Target/Status/Domain set per the project config.
                Use `triage-issue` to read the current state as JSON.
                Never guess a field value; leave unset and flag for the project-manager.

2. DECOMPOSE    If Type is Epic or Feature, it is NOT directly implementable.
                Decompose into leaf sub-issues before any work starts.
                Use `decompose-issue --dry-run` to preview, then apply.
                Only leaf Tasks/Bugs may enter the ready-queue.

3. SELECT       During MVP delivery, select ONLY issues where ALL hold:
                  Type ∈ {Task, Bug}                     (leaf, decomposable already done)
                  Target = MVP                            (Post-MVP is never scheduled in MVP)
                  Status = Ready                         (Definition of Ready met)
                  no unresolved Blocked-By               (native issue dependencies)
                  no conflicting in-flight PR            (same spec section / crate)
                Use `ready-queue` to list eligible issues as JSON.

4. CLAIM        Assign the issue to @me and advance Status to "In Progress".
                Use `claim-issue` (supports --dry-run).
                One active claim per agent; release before claiming another.

5. BLOCK        If work discovers a blocker (Arbitraitor upstream, conflicting PR,
                missing spec), set the issue's Blocked-By edge and Status="Blocked".
                Cross-repo blockers (Arbitraitor): open the issue in arbsec/arbitraitor,
                then use `create-blocker --repo arbsec/arbitraitor` to link from here.
                Do NOT retry a policy/Arbitraitor blocker as if it were transient (spec §9.26.1).

6. FOLLOW-UP    Independently testable/revertible discovered work gets its OWN issue,
                never hidden inside the current PR (spec §9.33.2).
                Use `create-follow-up`. Security/correctness defects needed for safe
                completion may NOT be deferred merely to shrink PR scope.

7. RELEASE      When abandoning (blocked, deprioritized, stale > claimed-lease):
                unassign @me, set Status back to "Ready", leave a comment explaining why.
                Use `release-issue`.
                Stale claims are reclaimed by the project-manager via the same script.

8. RECONCILE    State drift happens (humans edit in the UI, bots move items).
                When in doubt, read fresh state with `triage-issue` before mutating.
                This skill never caches mutable node IDs in committed files — see
                `graphql.md` for runtime resolution + caching outside the repo.
```

## Inputs

- **Issue identifier**: number, URL, or JSON from stdin (for piping).
- **Project config**: `.agents/project/github-project.local.toml` (copy of the example). If absent, scripts exit `2` with an actionable message — **never guess** org/project/field/option identity.
- **`gh` auth**: `gh auth status` must report logged-in with the `project` scope for field writes (`gh auth refresh -s project` if missing).

## Outputs

- Human-readable by default; `--json` for machine consumption.
- Mutating scripts print the resulting issue URL and the field/edge they changed.
- `--dry-run` prints the exact `gh`/GraphQL that would run, writes nothing, exits `0`.

## Failure states (stable exit codes)

| Code | Meaning |
|---|---|
| `0` | Success (or `--dry-run` preview rendered) |
| `1` | Unrecoverable error (network, auth, unexpected `gh` output) |
| `2` | Config/state error (missing project config, unknown field name, issue not found, field value not in allowed options) |
| `3` | Policy violation (would mutate a Post-MVP issue during MVP, would block on a non-existent issue, would claim a second active issue) |
| `4` | Concurrent edit detected (issue's `updatedAt` changed since read — re-read and retry) |

## Safety conditions (non-negotiable)

- **Never guess identity.** Repository, org, project number, field names, and option names all come from the project config or CLI flags — never inferred from defaults or issue bodies.
- **Validate before mutate.** Every mutating script reads current state, checks the precondition, then mutates. `claim-issue` refuses if Status ≠ "Ready"; `release-issue` refuses if the caller is not the assignee.
- **Dry-run first.** Remote-changing operations support `--dry-run` and print the exact commands/GraphQL they would execute.
- **Preserve concurrent human edits.** Scripts compare `updatedAt` before and after; a mismatch exits `4` with "re-read and retry" rather than silently overwriting.
- **No admin bypasses.** This skill never force-pushes, never bypasses review, never edits issues the caller cannot see.
- **Cross-repo Arbitraitor blockers are linked, not duplicated.** Open the canonical issue in `arbsec/arbitraitor`, then create a `Blocked-By` edge from the Orchestraitor issue (spec §16.2).

## How this skill relates to project policy

This skill's instructions describe **how** to manipulate GitHub issues generically. **Whether** a given transition is allowed for Orchestraitor is policy — see `.agents/project/orchestraitor-workflow.md` (MVP-only scheduling, required reviewer domains, Arbitraitor ownership boundaries). If the two ever disagree, project policy wins; update this skill.

## Keeping the skill current

- Re-verify the `gh` CLI surface against [`references/gh-capabilities.md`](references/gh-capabilities.md) when bumping `gh` versions. Sub-issues, issue types, and dependencies all require **gh v2.94.0+**.
- Update [`references/graphql.md`](references/graphql.md) if GitHub changes field/mutation names — these have shifted before (e.g. `blockedBy` landed Aug 2025).
- Keep this file **under 500 lines** (Agent Skills spec). Move new detail to a reference, not here.
