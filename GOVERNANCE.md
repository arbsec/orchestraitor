# Governance

## Status

Orchestraitor is currently maintained by its founding contributors. Governance will evolve as
the community grows.

## Decision making

### Technical decisions

Architecturally significant decisions are recorded as ADRs (Architecture Decision Records) in
`docs/adr/` once the repository grows one. Each ADR has a state: `Proposed`, `Accepted`,
`Superseded`, or `Rejected`. The product and architecture source of truth is
[`docs/spec/spec.md`](docs/spec/spec.md); the technology stack source of truth is
[`docs/spec/tech-stack.md`](docs/spec/tech-stack.md).

### Security decisions

Orchestraitor delegates all security policy and enforcement to Arbitraitor. Any change touching
the Arbitraitor integration boundary, a privilege grant, capability issuance, output promotion,
or a security invariant requires security-owner review and, where it changes security behavior,
human review before release (spec §21.1). Changes to security **invariants** themselves — i.e.
behavior Arbitraitor is responsible for — require maintainer consensus and are made in
`arbsec/arbitraitor`, not here.

## Roles

| Role | Responsibilities |
|------|-----------------|
| Contributor | Submits PRs, participates in discussions |
| Maintainer | Reviews PRs, merges changes, manages releases, triages the backlog |
| Security Owner | Reviews security-sensitive changes and the Arbitraitor integration surface; coordinates with Arbitraitor security owners |

> The `project-manager`/`spec-author`/`task-planner`/`implementer`/`reviewer`/`domain-reviewer`/
> `verifier` roles defined in spec §9.33.1 are **autonomous delivery roles**, not human
> governance roles. They configure how agents work; they do not grant merge authority.

## Teams

GitHub teams map to review boundaries (to be wired up as the project grows):

- `@arbsec/maintainers` — repository maintenance, CI, governance, workflow automation.
- `@arbsec/security` — the Arbitraitor integration boundary and any security-sensitive path.

> Orchestraitor does not define its own sandbox/policy/rule teams — those boundaries live in
> Arbitraitor and route to that repository's owners.
