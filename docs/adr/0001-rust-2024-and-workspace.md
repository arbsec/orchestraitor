# ADR 0001: Rust 2024 edition, workspace resolver 3, and MSRV 1.96.0

**Status:** Accepted
**Date:** 2026-07-30
**Issue:** #69

## Context

Orchestraitor is a security-first coding-agent harness secured by
[Arbitraitor](https://github.com/arbsec/arbitraitor). The language choice,
edition, toolchain pin, and workspace resolver affect memory safety, build
reproducibility, dependency compatibility, and CI correctness. Because
Orchestraitor consumes Arbitraitor crates via a pinned git dependency
(tech-stack §2.1), the toolchain and MSRV MUST match Arbitraitor exactly to
avoid silent API drift and duplicate-dependency conflicts.

## Decision

- **Language:** Rust, 2024 edition.
- **Bootstrap toolchain:** Rust 1.96.0, pinned in `rust-toolchain.toml`.
- **Workspace resolver:** Cargo resolver 3 (`resolver = "3"`).
- **Minimum Supported Rust Version (MSRV):** 1.96.0, matching Arbitraitor's
  MSRV. A rolling six-month MSRV will be considered only after public API and
  downstream users exist. Bumping the MSRV is a breaking change requiring a
  spec update and coordinated release with Arbitraitor.
- **Nightly:** prohibited for production features; used only in isolated CI
  jobs (Miri, sanitizer testing).
- **`Cargo.lock`:** committed for all crates in this monorepo.
- **Profiles:**
  - `release`: `codegen-units = 1`, `lto = "thin"`, `panic = "abort"`,
    `strip = "symbols"`.
  - `release-with-debug`: inherits release, `debug = 1`, `strip = "none"`.
- **Lints:** `unsafe_code = "forbid"` workspace-wide in core crates;
  `deny(missing_docs, unwrap_used, expect_used, panic, unimplemented,
  dbg_macro, print_stdout, print_stderr)`; `warn(clippy::pedantic,
  clippy::cargo)`. Local exceptions allowed with justification. This matches
  Arbitraitor's lint conventions (tech-stack §1, AGENTS.md "Rust conventions").

## Consequences

- Security-sensitive dependencies (Arbitraitor crates, TLS, parsers, sandbox
  primitives) can move quickly; pinning current stable avoids patch lag.
- `panic = "abort"` is appropriate for the shipped CLI and daemon; reusable
  library crates must not rely on process abortion as error handling.
- CI runs an additional job against current stable channel to detect drift.
- The MSRV lockstep with Arbitraitor means an Arbitraitor MSRV bump forces a
  coordinated Orchestraitor bump — this is intentional and keeps the two
  projects' shared types (`schemars 1.0`, `arbitraitor-*` crates) compatible.

## Alternatives considered

- **Older MSRV (e.g., 1.80):** Rejected. Creates patch lag for the Arbitraitor
  git dependency and security deps with little benefit for a pre-1.0 project.
- **Divergent MSRV from Arbitraitor:** Rejected. Arbitraitor crates are
  consumed via exact-rev git pin (tech-stack §2.1); a divergent MSRV risks
  duplicate-dependency resolution failures and silent API drift.
- **Resolver 2:** Rejected. Resolver 3 is the Cargo 2024 edition default and
  handles feature unification more correctly for the workspace's feature-gated
  provider crates.
- **C or C++:** Rejected. Memory safety risk in a security boundary.
- **Go:** Rejected. Less control over memory layout, unsafe code isolation,
  and zero-cost abstractions needed for the context compiler and control plane.

## References

- `docs/spec/tech-stack.md` §1 (Recommended baseline)
- `docs/spec/tech-stack.md` §2.1 (Hard constraint: no published crates)
- `docs/spec/spec.md` §2.2 (Security ownership invariant)
- `rust-toolchain.toml`
- `Cargo.toml` workspace section
- Arbitraitor [ADR 0001](https://github.com/arbsec/arbitraitor/blob/main/docs/adr/0001-rust-2024-and-toolchain-policy.md)
