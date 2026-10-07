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

Custom roles are configuration, not a hardcoded taxonomy (spec §9.22.4): any
`roles.<id>` key defined in a configuration layer registers a custom role that
resolves through the same path, the same layer semantics, and the same
fallback chain as a built-in. A role id must be 1–64 lowercase ASCII letters,
digits, `-` or `_`, starting with a letter or digit — never a path or key
separator. Configuration storage is shape-agnostic: a role id that fails these
shape rules can still be written via `orc config set`/`unset`, but it is
rejected at resolve time with the typed `InvalidRoleId` error — resolution is
the gate. A role id that is neither built-in nor configured is a typed error
listing the known roles.

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
- `routing.provider` — decision-provider selection (spec §9.45
  "DecisionProvider"), **default off**: when absent, the heuristic table
  above is the router and no decision model is consulted. When set to
  `fixture`, the deterministic table-driven `FixtureDecisionProvider`
  (in `orchestraitor-provider-api`) is consulted first for every role
  resolution; its typed proposal — provider, model, calibrated confidence
  (`0.0..=1.0`, validated at the boundary), and the alternatives it
  considered with skip reasons — wins over the table when the target
  provider is routable in the effective configuration. When set to
  `systemone`, the **System One decision protocol** is consulted (spec
  §9.45): a single-shot `POST <base_url>/systemone` request with typed
  questions and calibrated probabilities, zero generated text. System One
  is an open protocol served by multiple endpoints — the Neuralwatt cloud
  and self-hosted Clef inference engines alike are example deployments,
  never built-ins. It requires `routing.base_url` (below); an
  unresolvable credential is a typed startup error, not a silent
  fallback. Any other value is a typed `unknown decision provider`
  error naming the available implementations (`fixture`, `systemone`).
- `routing.base_url` — decision-endpoint base URL, **REQUIRED** for
  `systemone` (spec §10.3): any System One-compatible endpoint. Examples:
  the Neuralwatt cloud `https://api.neuralwatt.com/v1` or a self-hosted
  local Metal-native Clef engine such as
  `http://mekbook.tail1e276.ts.net:8080/v1`. The URL lives in
  configuration, never in code; there is no protocol-level default.
- `routing.model` — optional decision model id — any model the endpoint
  serves. Defaults to `clef-flash`. Confirm the exact id against the
  endpoint's model list (`GET /v1/models`) when pointing at a custom
  deployment.
- `routing.api_key` — optional credential reference for the decision
  endpoint, as a `secret://` URI (`secret://env/NEURALWATT_API_KEY` or
  `secret://keyring/neuralwatt`). Absent (or the literal `none`) means the
  endpoint takes no auth (a local/self-hosted deployment) and no
  `Authorization` header is sent. The resolved credential never enters an
  error or log line. Data classification of the endpoint (spec §9.28) is
  an operator choice through the `data_classification` rules — this key
  makes no assumption about what the endpoint may receive.

### Per-project decision-provider configuration

Decision-provider configuration is **per-project**: the `[routing]` block
reads through the same layered chain as every other key (built-in defaults
→ user/org layers → the project's `orchestraitor.toml`), and the project
layer wins field by field. A project without a `[routing]` block (in any
layer) keeps heuristic routing — no decision model is consulted and no
network call is made.

```toml
# <project>/orchestraitor.toml (project layer) — a local Clef inference
# engine (System One-compatible decision endpoint on the tailnet, no auth):
[routing]
provider = "systemone"
base_url = "http://mekbook.tail1e276.ts.net:8080/v1"
model = "clef-flash"
# api_key absent (or "none") → no Authorization header is sent.
```

```toml
# another project's orchestraitor.toml — the hosted Neuralwatt endpoint
# with an authenticated key:
[routing]
provider = "systemone"
base_url = "https://api.neuralwatt.com/v1"
model = "clef-flash"
api_key = "secret://env/NEURALWATT_API_KEY"
```

A third project sets no `[routing]` at all and keeps the deterministic
heuristic table. Every project picks its own endpoint/model/credential —
or none.

### Decision model defaults

When a decision provider is configured, decision calls route to
`clef-flash` by default (`routing.model` overrides it). The role the
decision calls themselves run as is not part of the role-routing table —
decision consultation is a control-plane surface, not a worker role — so no
`[roles.decision.routing]` entry is required or read.

### Decision-provider fallback

The heuristic table remains the fallback chain when a decision provider is
configured but unavailable (spec §9.45): provider errors, unknown fixture
roles, proposals failing identifier validation, or proposals naming a
non-routable provider all fall back to the table resolution, and the
unavailability is documented in the decision record's `fallback_reason`
(for example `decision provider 'fixture' unavailable: …; applied the
heuristic table fallback (spec 30-model-routing.md §9.45)`). A
`DecisionProvider`-proposed resolution records the provider and its
confidence in `precedence_path`
(`decision-provider:fixture (confidence 0.95, 1 alternative(s))`).
Confidence, structured alternatives, and per-alternative skip reasons land
as typed columns in a future `SCHEMA_V2` decision-store migration; the
current store keeps working unchanged.

The same chain applies to campaign task selection (`orc campaign run` and
`orc loop`): a configured provider is consulted with the eligible ready
task ids first, a well-formed proposal for an eligible task wins and the
campaign decision record names it in `precedence_path`
(`decision-provider:systemone (confidence 0.99)`), and any
provider error, a proposal for an id outside the eligible set, or an empty
ready queue falls back to the deterministic P0-first selection with the
cause recorded in the record's `rationale`.

Because layers merge field-wise, a project entry that sets only `provider`
inherits `model` from lower layers. A typed error naming the missing
configuration key (for example `roles.<role>.routing.model`) is produced only
when the *effective* configuration lacks the sub-key entirely — there is no
silent default-to-first-model guess anywhere in the chain. When a role has no
entry in any layer (library callers that resolve without the built-in defaults),
resolution falls back to the documented bootstrap default `neuralwatt`/`glm-5.2`
and the decision record's `fallback_reason` captures that fact.

## Custom roles

Define a custom role by adding a `roles.<id>` block in any layer — project
layer shown here:

```toml
# orchestraitor.toml (project layer)
[providers.acme]
endpoint = "https://example.invalid/v1"

[roles.migrator.routing]
provider = "acme"
model = "acme-pro"
```

The custom role resolves through the exact same path as a built-in: the same
`roles.<id>.routing.*` sub-keys, the same layer precedence and field-wise
merge, the same typed errors for partial entries and invalid values, and the
same bootstrap default when no entry resolves. Override and removal follow the
layer chain: a higher layer wins field by field, and removing the key from that
layer (`orc config unset roles.migrator.routing.provider --layer project`)
reveals the lower layer's value again.

## Precedence and conflicts

Layers resolve bottom-up (built-in defaults → user → org → project → dir).
Same-layer shards that set the same `roles.<id>.routing.*` key are rejected as
ambiguous by `orc config validate` before anything resolves — the error names
the key and both sources:

```text
ambiguous configuration conflict for key `roles.migrator.routing.provider`
from sources [".../orchestraitor.d/a.toml", ".../orchestraitor.d/b.toml"]
```

## Commands

```sh
orc config get roles.review.routing               # -> {model = ..., provider = ...}
orc config get roles.migrator.routing             # custom role, same shape
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
default) again. The decision-provider flag rolls back the same way:
`orc config unset routing.provider` re-disables the feature and the heuristic
table becomes the router again — no code or store migration is involved.
Decision records already persisted are append-only evidence and
are not rewritten.
