# ADR 0007: Independent per-control capability reporting

**Status:** Accepted
**Date:** 2026-06-30 (decision codified 2026-06-30; re-codified in this ADR on restoration 2026-10-13)
**Issue:** n/a (foundational posture; cited by the bootstrap-worker preflight and the daemon's capability report)

## Context

The startup probe (`probe_capabilities` in
`crates/orchestraitor-daemon/src/capability.rs`) reads Arbitraitor's
`EffectiveControls` matrix and produces a `CapabilityReport` for the
daemon's `health` JSON-RPC method. The matrix has seven distinct
controls (`filesystem_isolation`, `network_isolation`,
`process_tree_containment`, `privilege_suppression`, `syscall_filtering`,
`platform_settings_isolation`, `resource_limits`) — each with its own
`ControlState` (`Available`, `Degraded`, or `Unavailable`).

Two designs are possible:

1. **Aggregate flag.** Collapse the seven controls into one boolean:
   "is the daemon sandboxed?" Pass / fail.
2. **Per-control reporting.** Emit every control independently. Each
   control carries its own status, so the operator (and downstream
   automation) sees exactly which controls are present, which are
   degraded, and which are missing.

The aggregate-flag shape is simpler. The per-control shape is more
verbose but admits diagnostic use cases the aggregate cannot:

- A monitoring dashboard that wants to alert only on
  `network_isolation` regressions cannot do so from a single boolean.
- A test that needs to assert "Landlock is `Unavailable` but every
  other control is `Available`" cannot do so from a single boolean.
- A run-state record that needs to record the exact controls in effect
  for a given worker run cannot reconstruct that record from a boolean.

The bootstrap-worker preflight (`MediatedWorker::spawn`) and the
daemon's `health` method both consume `EffectiveControls` and both must
produce machine-readable, replayable evidence of what was actually in
effect when the run started.

## Decision

Report each required control independently. Never collapse them into a
single `sandboxed` flag.

Concretely:

- `required_capabilities_from` in
  `crates/orchestraitor-daemon/src/capability.rs:191` returns a
  `Vec<RequiredCapability>` with one entry per control. The seven
  controls are listed inline; each entry carries its own
  `RequiredCapabilityStatus` (`Available` / `Degraded` / `Unavailable`).
- The `CapabilityReport` (`capability.rs:179`) embeds the
  `required_capabilities` vector verbatim. The daemon's `health` JSON-RPC
  response (`crates/orchestraitor-daemon/src/rpc.rs:55`) surfaces the report
  to operators.
- The bootstrap-worker preflight's `WorkerPreflight`
  (`orchestraitor-arb-client/src/mediation.rs`) carries the same shape:
  a `controls` matrix and a `degraded_controls` vector, so a run-state
  record that persists the preflight output preserves every control's
  state.
- The test at `crates/orchestraitor-daemon/src/capability.rs:430` —
  `required_capabilities_reports_seven_independent_controls` — pins two
  properties of the contract: `caps.len() == 7` for all-available
  controls, and `caps.iter().all(|c| c.status == ControlStatus::Available)`.
  Any future PR that drops a control, collapses two into one, or
  substitutes a missing identifier for an empty placeholder breaks the
  count assertion. The test does NOT pin the exact identifier set (a
  duplicate identifier, or a rename to another valid identifier, would
  still satisfy both checks) — extending the test to assert the canonical
  identifier set is recorded as a follow-up, not part of this ADR.
- The `ControlStatus::from(ControlState)` mapping
  (`capability.rs:64`) is `match`-exhaustive over the three
  `ControlState` variants — adding a fourth `ControlState` is a
  compile-time error here, not a silent default.

## Consequences

- **Positive:** the operator-visible `health` report and the
  run-state `WorkerPreflight` record carry the same shape and the same
  semantics. A control that is `Available` in `health` is `Available` in
  the preflight that gates the next worker run.
- **Positive:** `Degraded` controls are reported distinctly from
  `Unavailable` controls. The aggregate-flag design would erase this
  distinction; the per-control design preserves it (spec
  `40-arbitraitor-integration.md` §6.7: "record the degradation").
- **Positive:** new controls (e.g. a future `seccomp_filtering` once
  Arbitraitor starts installing it on the worker path) are additive —
  the `Vec<RequiredCapability>` grows; the test that asserts
  `caps.len() == 7` becomes a test that asserts
  `caps.len() == 8` after the addition. The aggregate-flag design would
  silently extend the boolean's meaning.
- **Negative:** the `health` JSON-RPC response is larger than an
  aggregate flag. Operators reading the response with `jq` see a
  vector of seven entries plus the `missing_controls` and
  `degraded_mode` summaries; the wire format is intentionally not the
  smallest possible.
- **Negative:** the test surface has to cover the seven-entry contract
  and the three-variant exhaustive match. The trade is deliberate: the
  test is the mechanism that prevents a future "simplification" from
  collapsing two controls.
- **Recorded coupling:** the bootstrap-worker preflight
  (`MediatedWorker::spawn`, `crates/orchestraitor-arb-client/src/mediation.rs`)
  and the daemon's `health` (`crates/orchestraitor-daemon/src/capability.rs:191`)
  consume the same `EffectiveControls` matrix and share the canonical seven
  control identifiers, but they apply **separate translations** — the
  daemon maps to a `Vec<RequiredCapability>` for the JSON-RPC `health`
  report, the worker maps to `ControlNames` collections for
  `missing_controls` and `degraded_controls` on the `WorkerPreflight`
  record. The worker preflight additionally applies the
  `arbsec/arbitraitor#755` Landlock-ABI stopgap: when the kernel probe
  reports no Landlock ABI on the supported Linux platform, the worker
  treats `filesystem_isolation` as `Unavailable` even though Arbitraitor's
  matrix classifies it as `Available`; the daemon reports the matrix
  state directly without that override. If the identifier set, the
  per-path translation, or the Landlock-ABI override semantics diverge in
  the future, this ADR must be amended or superseded — not silently
  relaxed.

## Alternatives considered

- **Aggregate boolean `sandboxed: bool` with optional `missing: Vec<String>`.**
  Rejected: collapses `Degraded` into `Available` (the boolean is true
  if at least one control is `Available`, regardless of how many are
  degraded or missing) or into `Unavailable` (the boolean is false if
  any control is missing, erasing per-control diagnostics).
- **`CapabilityReport::status_str()` returning one of `"ready"`,
  `"degraded"`, `"fail_closed"`.** This is preserved as a one-line
  summary on top of the per-control vector (see `capability.rs`),
  never as a replacement for it.
- **Per-control boolean per control, no tri-state status.** Rejected:
  loses the `Degraded` distinctness that spec §6.7 requires the
  reports to record.

## References

- `crates/orchestraitor-daemon/src/capability.rs:64` — `ControlState`
  to `ControlStatus` conversion (`From<ControlState> for ControlStatus`).
- `crates/orchestraitor-daemon/src/capability.rs:191` —
  `required_capabilities_from`, the seven-entry translation.
- `crates/orchestraitor-daemon/src/capability.rs:431` — test section
  header that names this ADR.
- `crates/orchestraitor-daemon/src/rpc.rs:55` — `CapabilityReport`
  surface in the `health` JSON-RPC response.
- `crates/orchestraitor-arb-client/src/mediation.rs` — the bootstrap
  preflight that consumes the same `EffectiveControls` matrix and
  persists a structurally similar `WorkerPreflight`.
- Spec `40-arbitraitor-integration.md` §6.7 (record the degradation),
  §9.6 (Arbitraitor sandbox integration), §16.7 (capability report
  semantics), §27.7 (the seven-control taxonomy).
- `CHANGELOG.md:589`, `CHANGELOG.md:1052` — historical release-note
  anchors citing the bootstrap-worker fail-closed posture that depends
  on this ADR's per-control shape.
