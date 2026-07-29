<!--
  Orchestraitor pull-request template.
  Checkboxes are verified facts, not intentions. Every managed-list marker
  (<!-- orc:... -->) is machine-readable and consumed by the pr-lifecycle skill.
  See AGENTS.md "Review and merge invariants" for the gate these items enforce.
-->

## Summary

<!-- What does this PR do and why? One paragraph. -->

## Linked issue

<!-- Closes #N, Refs #N. The linked issue must carry Target = MVP, Status = Ready, and no
     unresolved blockers before this PR may merge (workflow policy). -->

## Specification references

<!-- docs/spec/spec.md §N and/or docs/spec/tech-stack.md §N that this change implements. Every
     implementation PR traces to a spec requirement (AGENTS.md). -->

## Change summary

<!-- Bullet list of the concrete changes, sufficiently detailed for a reviewer on a fresh
     context to follow without re-deriving the diff. -->

## Security impact

<!-- Required. Describe any security implications. Orchestraitor implements NO security
     primitive (spec §2.2, §16) — if this change needs new security behavior, it is blocked on
     an Arbitraitor issue/PR; link it here. -->

- [ ] No security impact
- [ ] Arbitraitor integration boundary touched (requires security-owner review)
- [ ] Security-sensitive path changed — requires human review before release (spec §21.1)
- [ ] Blocked on an Arbitraitor issue/PR: <!-- link it -->

## Test evidence

<!-- What tests were added/updated? For security-sensitive behavior, include negative and
     adversarial tests asserting the forbidden effect did not occur (spec §21.4). -->

- [ ] Unit tests added/updated
- [ ] Property tests added where applicable
- [ ] Negative/adversarial tests for security-sensitive behavior
- [ ] Regression test added for any defect found during this work

## Documentation impact

<!-- Any change to public behavior updates human-facing docs in the same PR (spec §9.17.1,
     §9.33). Generated API docs alone do not count. -->

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
- [ ] <!-- orc:tests --> Tests are added or updated
- [ ] <!-- orc:security --> Security impact is reviewed
- [ ] <!-- orc:docs --> Human-facing documentation is updated, or not required with justification
- [ ] <!-- orc:rollback --> Rollback or recovery behavior is verified where applicable
- [ ] <!-- orc:checks --> All required and non-optional checks pass
- [ ] <!-- orc:review --> All noteworthy findings are resolved
- [ ] <!-- orc:threads --> All actionable review threads are resolved
