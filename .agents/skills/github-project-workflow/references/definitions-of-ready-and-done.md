# Definitions of Ready and Done

Two gates, both enforced. `Ready` controls whether an issue may be claimed; `Done` controls whether a PR may merge.

## Definition of Ready (an issue may be claimed)

An issue is `Ready` only when ALL of:

- `Type ∈ {Task, Bug}` (leaf — Epics/Features must be decomposed first).
- `Target = MVP` during the MVP phase (`Next`/`Later`/`Icebox` are never scheduled).
- `Risk` is set (zero unset values — never schedule a "guess").
- No unresolved `blockedBy` (native issue dependency).
- No conflicting in-flight PR touching the same spec section / crate (AGENTS.md; spec §9.33.2).
- The body has:
  - a **single, bounded objective**;
  - an explicit **specification reference** (`docs/spec/spec.md §N` and/or `tech-stack.md §N`);
  - **acceptance criteria** that are observable and testable (spec §9.33.2);
  - **explicit non-goals** (what is deliberately out of scope);
  - **security impact** declared;
  - **testing requirements** identified (unit/property/integration; negative/adversarial for security-sensitive);
  - **documentation impact** identified;
  - **dependencies** listed (blocking issues, Arbitraitor upstream);
  - **rollback implications** noted.

If any field is unset or a criterion is missing, the issue stays `Backlog`, not `Ready`. Never guess a value to force `Ready` — escalate to the project-manager (spec §9.33.1).

## Definition of Done (a PR may merge)

A PR may merge only when ALL of (spec §9.33.4, AGENTS.md "Review and merge invariants"):

- All required and non-optional CI checks pass against the **current HEAD**. A missing/skipped check is a failure, not a pass (spec §21.10).
- All actionable review threads are resolved.
- All noteworthy findings (CRITICAL/HIGH/MEDIUM) are fixed or formally resolved with recorded reasoning. LOW findings may be deferred only with an explicit justification comment on the finding.
- Adversarial review converges against the current HEAD: one full review generation finds no new noteworthy findings AND all earlier blocking findings are resolved (see the pr-lifecycle skill's `pr-convergence` reference).
- Required documentation is updated in the same PR for any public-behavior change (spec §9.17.1, §9.33). Generated API docs alone do not count.
- The PR checklist items (the `<!-- orc:* -->` markers) are checked based on **evidence**, not intentions.

## Limits are safety valves, not convergence

Reaching a configured `max_review_loops`, cost, or elapsed-time limit produces a `blocked` or `needs-human` state (spec §9.24, §9.33.4). It **never** counts as successful convergence. The implementer adds a human reviewer and moves on; the issue/PR is not silently approved.

This rule is the difference between "an agent that ships when green" and "an agent that ships when it gives up". Orchestraitor requires the former.
