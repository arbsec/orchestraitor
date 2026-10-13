# ADR 0024: Linux-only bootstrap-worker mediation

**Status:** Accepted
**Date:** 2026-08-15 (decision codified 2026-08-15; re-codified in this ADR on restoration 2026-10-13)
**Issue:** #311 (bootstrap worker sandbox mediation)

## Context

The bootstrap worker (`crates/orchestraitor-worker`) is the smallest coding
agent Orchestraitor ships today: a headless one-shot mini-worker that runs
one task through a bounded loop with exactly four tools (read file, search,
Arbitraitor-mediated bash, path-confined write). It is also the path
Orchestraitor's own self-hosting loop uses — the loop is the first
consumer of the worker, and the worker is the first consumer of Arbitraitor
on Orchestraitor's side.

Two facts constrain the platform posture:

1. `arbitraitor_sandbox::SandboxMode::Restricted` (the containment envelope
   the bootstrap worker demands) is implemented for Linux only in the
   pinned Arbitraitor revision (`e36644e`, spec `40-arbitraitor-integration.md`
   §9.6 / §27.7). The controls — Landlock filesystem rules, `no_new_privs`,
   fd closure, network-namespace isolation, fenced `prlimit` resource
   caps — are kernel features that have no portable equivalent on macOS
   (no Landlock; `seatbelt` is a different API surface), Windows
   (no `no_new_privs`; job objects are not equivalent), or BSD
   (no Landlock; Capsicum is a different API surface).
2. Spec §6.7 and §16.2 say "Arbitraitor is the sole security authority;
   missing capabilities fail closed or run in an explicitly-labelled
   non-secure mode — never a silent duplicate." Spec §16.2 additionally
   names the bootstrap-worker mediation path as one where a non-secure
   mode is *not* acceptable.

The existing patterns Orchestraitor inherits from Arbitraitor distinguish
two cases: (a) a missing capability that has a documented non-secure mode
the operator explicitly enables, and (b) a missing capability with no
fallback. The bootstrap-worker mediation path is case (b): the rest of
Orchestraitor (the daemon, the MCP gateway, the board provider) has
typed errors and refuse-to-start semantics for missing controls, but it
also runs on macOS for development (the daemon's `probe_capabilities_on_macos_fails_closed`
test at `crates/orchestraitor-daemon/src/capability.rs:272` is precisely
this contract). The bootstrap worker takes the stricter position: no
fallback at all.

## Decision

The bootstrap worker runs on Linux only. Every other platform fails
closed with a typed `MediationError::UnsupportedPlatform`; no explicitly
labelled non-secure mode exists on this path.

Concretely:

- `crates/orchestraitor-arb-client/src/mediation.rs:84` declares
  `pub const WORKER_PLATFORM: &str = "linux";` as the single source of
  truth for the platform name consumed by the preflight.
- `MediatedWorker::spawn` performs the Arbitraitor preflight and gates on
  the platform string before any execution surface exists — no child
  process, no temporary execution directory, no run-state row.
- `MediationError::UnsupportedPlatform { platform }` (`mediation.rs:146`)
  is the typed refusal; its `Display` impl explicitly cites
  "ADR-0024 fails closed; no non-secure mode" so the diagnostic anchors
  the decision in machine-testable form
  (`mediation.rs:836`).
- `MediatedBashMediator::worker` (`crates/orchestraitor-worker/src/mediator.rs:60`)
  runs the preflight lazily on the first bash call, so a worker run that
  never invokes bash never probes the sandbox — but if it does, the gate
  fires before any execution surface exists.
- The gate refuses on platform mismatch *and* on missing controls
  (`MediationError::UnavailableControls { missing }`); missing controls
  carry the control identifiers in the diagnostic so the failure mode is
  actionable, not opaque.
- On Linux-without-Landlock, the preflight consumes Arbitraitor's own
  probe datum (`landlock_abi_version: None`) and records
  `filesystem_isolation` as missing; the gate refuses. This is the
  `#755 stopgap` (`mediator.rs:49`) until Arbitraitor classifies the
  Landlock-absent case natively.

## Consequences

- **Positive:** the bootstrap worker never claims a sandbox posture that
  the host cannot deliver. The run-state record's controls matrix is the
  truth, not an inferred value. The Landlock-absent stopgap closes the
  inverse-divergence direction (classification claims available while
  enforcement installs nothing — see the `[Landlock-absent]` section in
  `docs/sandbox-mediation.md`).
- **Positive:** the typed refusal's `Display` impl carries the
  `ADR-0024` anchor, so any log scraping, alert, or downstream CI check
  that greps the diagnostic keeps working without re-reading source.
- **Negative:** macOS / Windows developer ergonomics — there is no
  `cargo run -p orchestraitor-cli -- loop` on a Mac laptop. The daemon,
  the MCP server, and the board provider all run on macOS for development;
  the bootstrap worker does not. This is documented in
  `docs/sandbox-mediation.md` and in `README.md` line 250-252.
- **Negative:** any future cross-platform mediation effort (Arbitraitor
  adds Capsicum / `seatbelt` support, or the spec relaxes §16.2 for this
  path) requires amending or superseding this ADR; the constant
  `WORKER_PLATFORM = "linux"` is the line that has to move first.
- **Recorded gap:** spec §16.2 names this path fail-closed without a
  fallback, but does not enumerate the upstream Landlock-active wrapper
  bug ([arbsec/arbitraitor#754](https://github.com/arbsec/arbitraitor/issues/754))
  — see `docs/sandbox-mediation.md` "Known upstream gap: Landlock-active
  wrapper". Until upstream fixes the wrapper composition, network-isolated
  script execution is fail-closed-inoperative on Landlock-active hosts;
  the fix is owned by Arbitraitor per spec §16.2.

## Alternatives considered

- **Per-platform fallback with a labelled non-secure mode.** Rejected:
  spec §16.2 names the bootstrap-worker mediation path as one where no
  non-secure mode is acceptable, and the bootstrap loop is the loop that
  drives Orchestraitor's own self-hosting — a non-secure worker would
  silently accept self-modifications under a degraded label. The cost
  of a developer not being able to run `orc loop` on a Mac is much lower
  than the cost of an undetected isolation breach on the bootstrap path.
- **Platform detection with a refusal on platform AND on missing controls
  but a degraded-but-runnable mode when all controls are `Available`.**
  Rejected: the preflight already records `Degraded` controls (per spec
  §6.7: "record the degradation"); degraded controls do not refuse, they
  are persisted to run state. The platform-name refusal is independent of
  control-state reporting.
- **Move the Linux-only decision into Arbitraitor's preflight API.**
  Considered: `arbitraitor_sandbox::compute_effective_controls` could
  return an `UnsupportedPlatform` variant for non-Linux hosts and let
  consumers treat it uniformly with `UnavailableControls`. Rejected for
  now: the upstream API contract treats platform as a string parameter,
  not a typed variant, and the bootstrap worker's typed-error surface
  (with the `ADR-0024` anchor in the diagnostic) is more useful at the
  consumer boundary than a uniform enum variant that loses the platform
  context.

## References

- `crates/orchestraitor-arb-client/src/mediation.rs:84` — `WORKER_PLATFORM`
  constant.
- `crates/orchestraitor-arb-client/src/mediation.rs:146` —
  `MediationError::UnsupportedPlatform` (cites ADR-0024).
- `crates/orchestraitor-arb-client/src/mediation.rs:836` — diagnostic test
  asserting the `ADR-0024` anchor.
- `crates/orchestraitor-worker/src/mediator.rs:49` — lazy preflight note
  ("refuses on hosts without the required controls, per ADR-0024 and the
  #755 stopgap").
- `crates/orchestraitor-daemon/src/capability.rs:272` —
  `probe_capabilities_on_macos_fails_closed` test (the contrasting
  *daemon* posture — fail-closed with a typed error, not a missing-capability
  refusal; this ADR is the *stricter* sibling).
- `docs/sandbox-mediation.md` — operator-facing flow doc that names
  ADR-0024 and links the Landlock-active / Landlock-absent upstream gaps.
- `README.md:243` — the user-facing reference (lines 250-252 in the
  audit snapshot).
- `CHANGELOG.md:589`, `CHANGELOG.md:1052` — historical release-note
  anchors citing ADR-0024.
- `AGENTS.md` — always-active agent rule set; this ADR is the
  fail-closed posture referenced by the bootstrap-worker path.
- Spec `40-arbitraitor-integration.md` §6.7 (sole security authority,
  fail closed), §9.6 (Arbitraitor sandbox integration), §16.2 (missing
  capabilities fail closed — no duplicate enforcement).
