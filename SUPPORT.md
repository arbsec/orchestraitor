# Support

## Getting help

- **Questions and discussion:** [GitHub Discussions](https://github.com/arbsec/orchestraitor/discussions)
- **Bug reports:** [GitHub Issues](https://github.com/arbsec/orchestraitor/issues) (use the bug
  report template)
- **Security vulnerabilities:** see [SECURITY.md](SECURITY.md) — **do not use public issues for
  security reports**

## Before filing an issue

1. Search existing issues and discussions to avoid duplicates.
2. Confirm the behaviour against the latest `main` branch.
3. State which spec section the issue relates to (`docs/spec/spec.md` §N) — Orchestraitor is
   spec-driven, so an issue without a spec reference is harder to triage.
4. Redact all secrets, credentials, tokens, and signed URLs from any pasted output
   (spec §9.23.4).

## Specification vs implementation gaps

The repository is pre-implementation. If the specification and (future) code disagree, open an
issue describing the disagreement against the relevant spec section rather than assuming one is
correct (per `AGENTS.md`).
