# Security Policy

## Reporting a vulnerability

**Do not report vulnerabilities through public GitHub issues.**

Use [GitHub private vulnerability
reporting](https://docs.github.com/en/code-security/security-advisories/guidance-on-reporting-and-writing-information-about-vulnerabilities/privately-reporting-a-security-vulnerability)
to disclose responsibly. Please include:

- Description of the vulnerability and its impact.
- Steps to reproduce or proof of concept.
- Affected versions or commits.
- Suggested fix if available.

We will acknowledge receipt within 72 hours and aim to provide an initial assessment within
7 days.

## Where the work belongs

Orchestraitor delegates **all** security primitives and enforcement to
[Arbitraitor](https://github.com/arbsec/arbitraitor) (spec §2.2, §16). When reporting, identify
which repository owns the defect:

- **Arbitraitor** owns sandboxing, policy, command/package/plugin/artifact inspection, network
  and secret enforcement, plan-bound approvals, output classification, promotion
  authorization, provenance, and receipts. A vulnerability in a security mechanism belongs in
  `arbsec/arbitraitor`.
- **Orchestraitor** owns orchestration, adapters, the context compiler, transactional
  filesystem tools, and presentation of Arbitraitor decisions. A vulnerability in how
  Orchestraitor calls Arbitraitor, validates Arbitraitor availability before an action, or
  presents a decision to the user belongs here.

Orchestraitor issues may track integration work but MUST link to the canonical Arbitraitor
issue or pull request (spec §16).

## Security invariants

These are non-negotiable; any change that weakens them is rejected (spec §6, §16.2):

1. **No duplicate security authority.** Orchestraitor must not implement a sandbox, policy
   engine, enforcement layer, or fallback. Missing Arbitraitor capabilities fail closed or run
   in an explicitly-labelled non-secure mode (spec §6.7).
2. **No silent weakening.** Any security weakening must be explicit, visible, auditable, and
   limited to options Arbitraitor supports (spec §9.22.9).
3. **Never advertise a stronger guarantee than the active platform backend can enforce**
   (spec §9.32.6). The provider-proxy mode does not contain tools the external harness executes
   outside Orchestraitor/Arbitraitor (spec MVP-3).
4. **MCP annotations are advisory, not proof.** `readOnly`/`destructive`/`idempotent` are input
   to policy; authority comes from Arbitraitor's analyzer (spec §9.18.1).
5. **The default Arbitraitor MCP stdio server is inspect-only.** Approve/Execute tools require
   explicit `McpServer` construction with injected dependencies; treating the default server as
   providing them is a security-critical bug (spec §9.9, tech-stack.md §2.3).

## Supported versions

| Version | Supported |
|---------|-----------|
| < 1.0   | Security fixes only on latest `main` |

Orchestraitor is pre-implementation. Only the latest `main` branch receives security fixes.
