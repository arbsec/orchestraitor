# `orc routing` — role-based model routing

Static heuristic role routing for the bootstrap milestone, per
[§9.45](../spec/30-model-routing.md#945-role-based-model-routing) (role-based model
routing). A role is a phase of work in the orchestration loop; the control plane —
never the worker — resolves each role to a `(provider, model)` pair through the
layered configuration (spec §9.22.2), following the routing sub-key semantics of
spec §9.19.2.

## Built-in roles

| Role | Phase of work |
| --- | --- |
| `explore` | Read-only context gathering over the codebase. |
| `research` | External context gathering — documentation, upstream sources. |
| `plan` | Producing or revising a work plan. |
| `implement` | Producing or modifying code. |
| `review` | Critiquing existing code or a diff. |
| `verify` | Running and interpreting required checks. |

Custom roles are not part of the bootstrap; an unknown role id is a typed error
listing the built-in roles. Role registry generalization deepens in E2.

## Configuration

Routing entries are shipped as built-in default TOML data (spec §9.22.1), one
block per role:

```toml
[roles.implement.routing]
provider = "neuralwatt"
model = "glm-5.2"
```

All keys participate in the normal precedence chain, so a project (or any higher
layer) overrides the built-in default field by field:

- `roles.<role>.routing.provider` — provider identifier. Must match the provider
  id shape (lowercase ASCII letters, digits, `-` or `_`, 1–64 characters, starting
  with a letter or digit) and name a provider declared under `providers.*` in the
  effective configuration — the single-provider bootstrap target `neuralwatt`
  (spec §10.3) is always routable so fresh projects resolve without a provider
  block. An unconfigured provider is a typed error naming the key, never a silent
  guess.
- `roles.<role>.routing.model` — model identifier. Must match the model id shape
  (ASCII letters, digits, `-`, `_`, `.`, `/`, `:`, 1–256 characters, starting with
  a letter or digit); violations are a typed error naming the key.
- `roles.<role>.routing.profile` — optional profile name, reserved.

Because layers merge field-wise, a project entry that sets only `provider`
inherits `model` from lower layers. A typed error naming the missing
configuration key (for example `roles.<role>.routing.model`) is produced only
when the *effective* configuration lacks the sub-key entirely — there is no
silent default-to-first-model guess anywhere in the chain. When a role has no
entry in any layer (library callers that resolve without the built-in defaults),
resolution falls back to the documented bootstrap default `neuralwatt`/`glm-5.2`
and the decision record's `fallback_reason` captures that fact.

## Commands

```sh
orc config get roles.implement.routing.provider   # -> neuralwatt (built-in default)
orc routing resolve --role <id> [--json]          # resolve + persist a decision record
```

`orc routing resolve` prints the resolved role, provider, model, precedence path
(which layer supplied each field, or `bootstrap-default`), and the fallback
reason (`none` when a table entry matched directly). With `--json` the same data
is emitted as a stable object that includes the persisted record id.

## Decision records

Every resolution is persisted to the local SQLite store at
`<config-dir>/routing.db` (default `.orchestraitor/routing.db`) with the
`role_routing_decisions` table:

| Column | Content |
| --- | --- |
| `role` | Resolved orchestration role id |
| `provider` | Selected provider id |
| `model` | Selected model id |
| `precedence_path` | Layer attribution for each field, or `bootstrap-default` |
| `fallback_reason` | Documented fallback reason, `NULL` for direct matches |
| `created_at` | RFC 3339 UTC insert timestamp |

Records are replayable evidence: given the same layered configuration, the same
role resolves to the same record. The store is schema-versioned via a
`schema_migrations` table so E2's `DecisionProvider` additions (alternatives
considered, per-alternative skip reasons, provider confidence) land as additive
migrations.

## Rollback

The table is config-only: `orc config unset roles.<role>.routing.<key>` removes
an override from the active layer and reveals the lower layer (or the built-in
default) again. Decision records already persisted are append-only evidence and
are not rewritten.
