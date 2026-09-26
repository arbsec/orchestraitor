#!/usr/bin/env python3
"""E2 leaf tasks: model routing + DecisionProvider + subscription awareness."""

TASKS = [
    {
        "id": "E2-T1",
        "epic": "E2",
        "title": "Task: routing-registry — role registry (built-in six + custom roles via layered config)",
        "risk": "Low",
        "estimate": 3,
        "labels": [],
        "blocked_by": [],
        "body": """## Objective

The role registry: built-in roles (explore, research, plan, implement,
review, verify) plus user-defined custom roles, held as configuration (not a
hardcoded taxonomy); roles compose with the §9.19.1 (domain, role) catalog
and resolve through §9.19.2 precedence (`roles.<id>.routing.*` sub-keys carry
the same layer semantics as `agents.domains.<id>.routing.*`).

## References

- `docs/spec/30-model-routing.md` §9.45 (role-based model routing — registry)
- `docs/spec/30-model-routing.md` §9.19.1 (domains, roles, generic fallback)
- `docs/spec/30-model-routing.md` §9.19.2 (precedence)
- `docs/spec/50-contracts-data.md` §9.22 (layered configuration)

## Acceptance criteria

- [ ] All six built-in roles are defined and resolvable; a custom role defined in a config layer resolves identically
- [ ] Precedence test: layer override changes a role's resolution; removal falls back correctly
- [ ] `cargo nextest run --workspace` green including registry unit tests

## QA scenarios

- Happy: `orc config get roles.review.routing` resolves for built-in and custom roles; evidence `.omo/evidence/<task-slug>/registry-resolve.txt`
- Failure: ambiguous same-layer conflict (two shards defining the same role key) -> `orc config validate` rejects it (§9.22.3 semantics); evidence `.omo/evidence/<task-slug>/registry-conflict.txt`

## Non-goals

No router logic (E2-T2); no DecisionProvider (E2-T4).

## Security impact

None (configuration surface).

## Testing requirements

Unit tests over layer fixtures; conflict-validation negative.

## Documentation impact

Config reference for `roles.*`; CHANGELOG `[Unreleased]`.

## Dependencies

None.

## Rollback implications

Config-only; `orc config unset` per key.
""",
    },
    {
        "id": "E2-T2",
        "epic": "E2",
        "title": "Task: routing-heuristic — heuristic router (static table, §9.19.2 precedence, fallback chain)",
        "risk": "Low",
        "estimate": 5,
        "labels": [],
        "blocked_by": ["E2-T1"],
        "body": """## Objective

The default router: a static heuristic table — role -> `(provider, model)`
entries resolved through layered configuration and the §9.19.2 precedence
chain, with the documented fallback chain when entries are missing. The
heuristic table remains the fallback when a `DecisionProvider` is configured
but unavailable. Workers never select their own model — the control plane
resolves `(provider, model, routing_reason)` and records it per call.

## References

- `docs/spec/30-model-routing.md` §9.45 (heuristic table first; fallback chain)
- `docs/spec/30-model-routing.md` §9.19.2 (precedence)

## Acceptance criteria

- [ ] Role resolution returns `(provider, model, routing_reason)` with the precedence path recorded
- [ ] Missing-entry fallback follows the documented chain (role -> domain default -> generic fallback) and records which step fired
- [ ] Determinism: same config + ledger state -> same resolution (replay test)
- [ ] `cargo nextest run --workspace` green including router unit tests

## QA scenarios

- Happy: each built-in role resolves; a role with a partial table walks the fallback chain with reasons; evidence `.omo/evidence/<task-slug>/heuristic-resolve.txt`
- Failure: no resolution possible (empty config) -> typed routing failure, no default-to-first-model guess; evidence `.omo/evidence/<task-slug>/heuristic-unresolvable.txt`

## Non-goals

No decision model; no subscription gating (E2-T6).

## Security impact

None (selection only).

## Testing requirements

Precedence + determinism unit tests; unresolvable-role negative.

## Documentation impact

Routing docs; CHANGELOG `[Unreleased]`.

## Dependencies

- Blocked by E2-T1 (registry).

## Rollback implications

Config-driven; table removable.
""",
    },
    {
        "id": "E2-T3",
        "epic": "E2",
        "title": "Task: routing-records — persisted, replayable routing decision records",
        "risk": "Low",
        "estimate": 3,
        "labels": [],
        "blocked_by": ["E2-T2"],
        "body": """## Objective

Every routing resolution is persisted as a decision record: resolved
`(provider, model)`, the precedence path that produced it, alternatives
considered, and per-alternative skip reasons (including
`skipped-because-quota`). Records are replayable: given the same board
state, configuration, and ledger state, a resolution is deterministic and
auditable.

## References

- `docs/spec/30-model-routing.md` §9.45 (routing decision records)
- `docs/spec/10-orchestrator.md` §9.35 (decision-record shape the campaign consumes)

## Acceptance criteria

- [ ] Each resolution writes a record with provider, model, precedence path, alternatives[], skip reasons
- [ ] Replay: re-resolving with recorded inputs reproduces the same decision (test)
- [ ] `cargo nextest run --workspace` green including record unit tests

## QA scenarios

- Happy: resolve + replay -> identical records; evidence `.omo/evidence/<task-slug>/record-replay.txt`
- Failure: a record with missing precedence path fails validation on write (typed), not silently; evidence `.omo/evidence/<task-slug>/record-invalid.txt`

## Non-goals

No analytics/UI beyond storage.

## Security impact

Records must not contain secrets (§9.23.4 redaction rules apply to any provider metadata logged).

## Testing requirements

Replay determinism tests; secret-redaction negative (fixture with a token in provider metadata -> redacted).

## Documentation impact

Record schema docs; CHANGELOG `[Unreleased]`.

## Dependencies

- Blocked by E2-T2 (router produces resolutions).

## Rollback implications

Append-only local state; deletable with the run store.
""",
    },
    {
        "id": "E2-T4",
        "epic": "E2",
        "title": "Task: routing-decisionprovider — `DecisionProvider` trait (typed structured outputs, calibrated confidence)",
        "risk": "Medium",
        "estimate": 5,
        "labels": [],
        "blocked_by": [],
        "body": """## Objective

The `DecisionProvider` trait in provider-api: a decision model returns typed
structured outputs with calibrated confidence — not string generation. A
`DecisionProvider` proposes role-to-model resolutions and the campaign
session's task-selection decision; the heuristic table stays the default
and the fallback chain. The transport type must not assume message streams
(new provider class).

## References

- `docs/spec/30-model-routing.md` §9.45 (DecisionProvider)
- `docs/spec/30-model-routing.md` §10.2 (provider transport architecture — new class must fit)
- Ledger F18 (jev/TypeSafe System One: typed outputs, parallel sampling, calibrated probabilities — vendor claims)

## Acceptance criteria

- [ ] The trait is defined in `orchestraitor-provider-api` with typed output + confidence fields; `cargo doc` clean
- [ ] A fixture DecisionProvider (deterministic, table-driven) implements it and plugs into the router behind a config flag (default off)
- [ ] `cargo nextest run --workspace` green including trait + fixture tests

## QA scenarios

- Happy: fixture provider proposes resolutions; records carry provider + confidence; heuristic stays default; evidence `.omo/evidence/<task-slug>/fixture-provider.txt`
- Failure: provider unavailable/erroring -> fallback chain engages (heuristic), unavailability recorded in the record; evidence `.omo/evidence/<task-slug>/provider-fallback.txt`

## Non-goals

No TypeSafe/jev adapter (E2-T5); no live decision-model dependency.

## Security impact

New provider class: untrusted outputs are typed data; no authority granted by a proposal.

## Testing requirements

Trait conformance via fixture; fallback negative.

## Documentation impact

Provider-api docs; CHANGELOG `[Unreleased]`.

## Dependencies

None (parallel to E2-T2; the router consumes it when configured).

## Rollback implications

Default-off feature; config flag removal disables.
""",
    },
    {
        "id": "E2-T5",
        "epic": "E2",
        "title": "Task: routing-jev — TypeSafe/jev adapter (fixture-tested, default-off until license allowlisted)",
        "risk": "Medium",
        "estimate": 5,
        "labels": [],
        "blocked_by": ["E2-T4"],
        "body": """## Objective

The reference `DecisionProvider` adapter for the TypeSafe/jev "System One"
model: typed outputs, parallel sampling, calibrated probabilities. The
adapter is fixture-tested only (no live dependency), default-off, and stays
off until the license is allowlisted per the dependency policy
(tech-stack §17). No hard dependency on TypeSafe/jev exists anywhere in the
workspace.

## References

- `docs/spec/30-model-routing.md` §9.45 (jev as reference adapter shape; default-off)
- `docs/spec/tech-stack.md` §17 (license status: not yet allowlisted) and §18 (dependency policy)
- Ledger F18 (jev properties; no Rust SDK — transport must not assume string generation)

## Acceptance criteria

- [ ] The adapter implements `DecisionProvider` over recorded fixtures (typed outputs + confidence); zero live-network tests
- [ ] The adapter is gated behind a default-off config flag; with the flag off, no jev code path executes
- [ ] `cargo nextest run --workspace` green; `cargo deny check` clean (no TypeSafe dependency added while un-allowlisted)
- [ ] A documented enable-path exists for when the license is allowlisted (config + policy note)

## QA scenarios

- Happy: fixture-driven adapter test produces typed resolutions with confidence; records carry the provider tag; evidence `.omo/evidence/<task-slug>/jev-fixture.txt`
- Failure: flag off -> router uses heuristic table only (assert no adapter call); malformed fixture output -> typed parse failure, fallback engages; evidence `.omo/evidence/<task-slug>/jev-off-and-malformed.txt`

## Non-goals

No live API integration; no license decision (owner-level, per policy).

## Security impact

Untrusted typed outputs; no credentials while default-off.

## Testing requirements

Fixture conformance; default-off negative; malformed-output negative.

## Documentation impact

Adapter docs + enable-path note; CHANGELOG `[Unreleased]`.

## Dependencies

- Blocked by E2-T4 (trait).

## Rollback implications

Default-off; flag removal fully disables.
""",
    },
    {
        "id": "E2-T6",
        "epic": "E2",
        "title": "Task: routing-subscription — subscription-aware routing (usage state, eligibility gate, all-exhausted stop)",
        "risk": "Medium",
        "estimate": 5,
        "labels": [],
        "blocked_by": ["E2-T2"],
        "body": """## Objective

Subscription-aware routing per §9.46: the cost ledger tracks per-subscription
usage state (`measured`, `estimated`, or `user-configured`) as the
eligibility input; the gate prefers subscription-backed instances with
remaining usage, SKIPS exhausted subscriptions when an alternative offers a
similar model (recorded as `skipped-because-quota` in alternatives[]); when
ALL subscriptions offering a suitable model are exhausted, the router stops
spawning that role's work and emits a needs-human signal — never a silent
fall-through to metered paid spend (falling through is an explicit recorded
configuration choice).

## References

- `docs/spec/30-model-routing.md` §9.46 (subscription-aware routing)
- `docs/spec/30-model-routing.md` §9.19.4-§9.19.6 (cost and subscription ledger, caps, budget scopes)
- Ledger D6 (subscription spend guards; first subscription provider: Neuralwatt)

## Acceptance criteria

- [ ] Usage state per subscription is tracked in the cost ledger with its source kind (measured/estimated/user-configured)
- [ ] Eligibility gate: exhausted subscription + alternative available -> alternative chosen, skip recorded with reason `skipped-because-quota`
- [ ] All-exhausted: no alternative -> role's work stops + needs-human signal; no metered fall-through unless explicitly configured (and then recorded)
- [ ] `cargo nextest run --workspace` green including gate unit tests

## QA scenarios

- Happy: mixed fixture (one subscription with quota, one exhausted) -> eligible one chosen, skip reason recorded; evidence `.omo/evidence/<task-slug>/gate-eligible.txt`
- Failure: all exhausted -> needs-human, no spawn, no spend; explicit-override config -> fall-through happens only when configured and is recorded in the decision record; evidence `.omo/evidence/<task-slug>/all-exhausted.txt`

## Non-goals

No provider-side quota API integration (user-configured windows suffice for v1); no monetary budget enforcement (E8).

## Security impact

None (eligibility logic); interacts with spend safety (E8-T3).

## Testing requirements

Gate unit tests; all-exhausted + explicit-override negatives.

## Documentation impact

Subscription config docs; CHANGELOG `[Unreleased]`.

## Dependencies

- Blocked by E2-T2 (router).

## Rollback implications

Config-driven; gate removable via config.
""",
    },
]
