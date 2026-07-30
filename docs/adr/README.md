# Architecture Decision Records

An ADR captures a decision that is architecturally significant, security-sensitive,
or expensive to change later. Each ADR is immutable once accepted; a new ADR must
supersede it.

## States

- **Proposed** — draft, open for discussion
- **Accepted** — decision is final and binding
- **Superseded** — replaced by a later ADR (reference included)
- **Rejected** — considered but not adopted (reasons recorded)

## Index

| ADR | Title | Status | Issue |
|-----|-------|--------|-------|
| [0001](0001-rust-2024-and-workspace.md) | Rust 2024 edition, workspace resolver 3, and MSRV 1.96.0 | Accepted | #69 |
| [0002](0002-arbitraitor-sole-security-authority.md) | Arbitraitor is the sole security authority | Accepted | #69 |
| [0003](0003-snapshot-workspace-default.md) | Snapshot workspace as the default mode | Accepted | #69 |
| [0004](0004-domain-agent-catalog.md) | Domain agent catalog (8 domains × 5 roles) | Accepted | #69 |
| [0005](0005-rmcp-and-acp.md) | rmcp for MCP and agent-client-protocol for ACP | Accepted | #69 |
| [0006](0006-explicit-arbitraitor-mcp-wiring.md) | Explicit Arbitraitor MCP wiring for Approve and Execute | Accepted | #69 |

## Format

```markdown
# ADR NNNN: Title

**Status:** Accepted | Proposed | Superseded by ADR-XXXX | Rejected
**Date:** YYYY-MM-DD
**Issue:** #NN (GitHub issue this ADR resolves, if applicable)

## Context

Why this decision is needed.

## Decision

What was decided.

## Consequences

What follows from the decision.

## Alternatives considered

Options that were evaluated and rejected.

## References

Spec sections, advisories, standards, library docs.
```
