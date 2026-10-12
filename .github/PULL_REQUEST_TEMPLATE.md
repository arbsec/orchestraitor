<!--
  Orchestraitor pull-request template.
  Checkboxes are verified facts, not intentions. Every managed-list marker
  (<!-- orc:... -->) is machine-readable and consumed by the pr-lifecycle skill.
  See AGENTS.md "Review and merge invariants" for the gate these items enforce.
-->

## Summary

<!-- 1-3 sentences: what this PR does and why. No code blocks. -->

## Closes

<!-- Exactly ONE primary closing keyword on its own line, early in the body:
     Closes #N. (Not Fixes/Resolves — single-`Closes` usage is project policy
     for a consistent primary reference; the `pr-labels-from-issue` extractor
     itself also accepts `Fixes`/`Resolves` forms.) Bare #N mentions are
     invisible to auto-close and to pr-labels-from-issue. For partial
     landings, use a bare `Refs #N` line and put the scope note on the NEXT
     line. Use `Closes #N` only when the whole tracked scope lands in this
     PR — a closing keyword triggers auto-close on merge. -->

## Specification references

<!-- docs/spec/<document>.md sections and/or docs/spec/tech-stack.md sections
     that this change implements (legacy section numbers resolve via
     docs/spec/spec.md). NEVER in the title. Format as a table (File | Change)
     or a linked list. The PR title is plain Conventional Commits
     `type(scope): description`, ASCII only (no section signs, em/en dashes,
     arrows, or smart quotes) with no parenthetical issue/spec refs; the only
     allowed parenthetical is `(re-land of #N)`. -->

| File | Change |
|---|---|

## Change

<!-- Bullet list; wrap every identifier, path, config key, symbol, SHA, and
     label in inline code so a fresh-context reviewer can follow the diff
     without re-deriving it. -->

-

## Design summary

<!-- Features and design-changing spec PRs only; omit for small fixes. -->
<!-- Bullets, not a prose wall (each bullet roughly 60 words or fewer).
     Rejected alternatives: one sentence each. -->

-

## Test evidence

<!-- Named tests as checkbox items, each with a one-clause assertion. A
     checked box is a verified fact at the time of writing, never aspirational.
     Security-sensitive PRs include negative/adversarial test names. -->

- [ ]

## Review convergence

<!-- Reviewed feature PRs only; omit for small fixes. Cite zero-findings
     generations with BOTH anchors: the reviewed head SHA and the base OID. -->

| Gen | Head SHA | Base OID | Findings | Remediation |
|---|---|---|---|---|

## Gates

<!-- For any PR with CI: the exact commands run and their results. Attribute
     pre-existing environmental failures (reproduced on clean origin/main). -->

## Security impact

<!-- Required. Describe affected security boundaries and implications, or state
     that there is no impact. Orchestraitor implements NO security primitive —
     new security behavior is blocked on an Arbitraitor issue/PR; link it. -->

## Documentation impact

- [ ] No public-behavior change
- [ ] Public behavior changed — docs updated (README, CLI/config reference, CHANGELOG)
- [ ] Not required — justification:

## Rollback implications

<!-- How is this change reverted or recovered if it goes wrong? (spec §9.14, §9.24.2) -->

- [ ] Independently revertible
- [ ] Rollback/recovery behavior verified where applicable

## Newly discovered follow-up work

<!-- Per spec §9.33.2: do NOT hide newly discovered work inside this PR. List any follow-up
     issues opened here (or state "none"). -->

## Merge checklist

<!-- The pr-lifecycle skill reads the orc:* markers below. Checkboxes are verified facts. -->

- [ ] <!-- orc:issue --> Linked issue and specification requirements are satisfied
- [ ] <!-- orc:service-identity --> All GitHub writes (PR creation, comments, review posts, merges, commits) attributed to arbsec-agent — no personal-account (@mekwall) writes (owner mandate; .agents/project/orchestraitor-workflow.md "GitHub service identity")
- [ ] <!-- orc:tests --> Tests are added or updated
- [ ] <!-- orc:security --> Security impact is reviewed
- [ ] <!-- orc:docs --> Human-facing documentation is updated, or not required with justification
- [ ] <!-- orc:rollback --> Rollback or recovery behavior is verified where applicable
- [ ] <!-- orc:checks --> All required and non-optional checks pass
- [ ] <!-- orc:review --> All noteworthy findings are resolved
- [ ] <!-- orc:threads --> All actionable review threads are resolved
