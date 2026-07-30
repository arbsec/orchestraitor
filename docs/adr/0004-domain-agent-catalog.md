# ADR 0004: Domain agent catalog (8 domains × 5 roles)

**Status:** Accepted
**Date:** 2026-07-30
**Issue:** #69

## Context

Spec §9.19.1 defines the agent catalog as the user-facing shape of sub-agent
orchestration. A `domain` is a technical specialty; a `role` is a phase of
work. An agent invocation has both: `(domain, role, provider, model)`. Domains
and roles are orthogonal — one domain can serve any role.

The catalog must be broad enough to route work to the right specialty, but it
must not encode per-brand agent taxonomies (e.g., named agents like "sisyphus",
"oracle", "metis") that couple the architecture to a specific product's naming
convention and obscure what the agent is actually doing.

The `security` domain requires special treatment: it is analysis-only and
MUST NOT implement security enforcement independently (spec §9.19.1, §2.2).

## Decision

### Eight built-in MVP domains

```text
general         Required generic fallback. Every project has it.
frontend        Web frontend, styling, accessibility, browser runtimes.
backend         Server, services, APIs, persistence, message buses.
data            Pipelines, schemas, migrations, analytics, ML serving.
devops          CI/CD, infrastructure, packaging, release engineering.
testing         Test design, fixtures, property tests, regression suites.
documentation   Prose, reference, examples, README, ADRs.
security        Security analysis and guidance. Analysis only — never enforcement.
```

The catalog is extensible through configuration and plugins. `orc init`
enables only the domains it detected as relevant for the repository; it MUST
NOT instantiate every built-in agent for every project. The `general` domain
is always enabled.

### Five built-in MVP roles

```text
planning        Producing or reviewing a work plan.
implementing    Producing or modifying code.
reviewing       Critiquing existing code or a diff.
testing         Designing or running tests.
researching     Gathering context (local codebase or external docs).
```

### No per-brand taxonomy

The catalog uses `(domain, role)` as the routing key — not branded agent names
like "sisyphus", "oracle", or "metis". Branded names:

- couple the architecture to a specific product's naming convention;
- obscure what the agent is actually doing in a turn (a name like "oracle"
  does not say whether it is planning, reviewing, or researching);
- make per-domain and per-role model routing (§9.19.2) harder to express,
  because the routing key is a brand, not a specialty × phase.

The `(domain, role)` pair is the routing key. Model resolution follows the
precedence in §9.19.2: explicit agent override → domain + role override →
domain default → role default → project default → global default.

### Security domain is analysis-only

The `security` domain is for security-focused analysis and guidance only. It
MUST NOT implement security enforcement independently. All security primitives
— policy, sandboxing, approvals, provenance, command/script/package analysis,
output promotion, secret brokering, receipts — MUST come from Arbitraitor
(spec §2.2, §9.6–§9.14, §16). Where a security gap exists for Orchestraitor's
workloads, it MUST be implemented in `arbsec/arbitraitor` first (spec §16.2);
the `security` domain agent never substitutes for an absent Arbitraitor
capability.

### Capabilities are request shapes, not grants

Each domain agent declares a manifest (spec §9.19.3) with capability request
shapes (`filesystem`, `network`, `shell`, `prompt_tools`). These are request
shapes, not grants. Arbitraitor's `CapabilitySet`
(`arbitraitor_plugin_api::CapabilitySet`) and resource limits are the
authoritative grant. The Orchestraitor side of the budget declares intent;
Arbitraitor enforces.

## Consequences

- Work is routed by specialty × phase, not by brand. The routing key is
  self-documenting: `(frontend, implementing)` says exactly what the agent is
  doing.
- The catalog is extensible: new domains and roles can be added through
  configuration and plugins without changing the routing architecture.
- `orc init` enables only relevant domains, avoiding unnecessary agent
  instantiation and token spend.
- The `security` domain agent provides analysis and guidance but cannot
  become a parallel security authority — it depends on Arbitraitor for all
  enforcement.
- Per-domain and per-role model routing (§9.19.2) composes cleanly because the
  routing key is `(domain, role)`, not a flat brand name.

## Alternatives considered

- **Per-brand agent taxonomy (sisyphus/oracle/metis):** Rejected. Couples the
  architecture to a product's naming convention, obscures the agent's actual
  phase of work, and complicates per-domain × per-role model routing.
- **Single "general" agent with no domain routing:** Rejected. Cannot route
  work to the right specialty; every task uses the same context and model
  regardless of whether it is frontend, backend, security, or documentation.
- **Security domain with enforcement capability:** Rejected. Violates spec §2.2.
  The security domain is analysis-only; enforcement comes from Arbitraitor.
- **Fixed catalog with no extension:** Rejected. Spec §9.19.1 requires the
  catalog to be extensible through configuration and plugins.

## References

- `docs/spec/spec.md` §9.19.1 (Domains, roles, and the generic fallback)
- `docs/spec/spec.md` §9.19.2 (Per-domain and per-role model routing)
- `docs/spec/spec.md` §9.19.3 (Agent manifest)
- `docs/spec/spec.md` §9.19.4 (Cost and subscription ledger)
- `docs/spec/spec.md` §2.2 (Security ownership invariant)
- `docs/spec/spec.md` §16.2 (Missing capabilities added to Arbitraitor first)
- `docs/spec/tech-stack.md` §2.2 (`arbitraitor_plugin_api::CapabilitySet`)
