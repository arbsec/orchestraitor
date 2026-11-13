# Bootstrap worker sandbox mediation

The bootstrap mini-worker's execution path runs only behind Arbitraitor
([§9.6](spec/40-arbitraitor-integration.md#96-arbitraitor-sandbox-integration),
[§6.7](spec/40-arbitraitor-integration.md#67-arbitraitor-is-the-sole-security-authority);
issue #311). Orchestraitor probes, records, and gates; Arbitraitor enforces.
Nothing on this path implements a security primitive.

The flow, all in `orchestraitor_arbitraitor_client::mediation`:

```text
MediatedWorker::spawn(client, platform)
  └─ probe_worker_preflight            arbitraitor_sandbox::compute_effective_controls
       (SandboxMode::Restricted, platform) → WorkerPreflight { controls matrix, verdict }
  └─ gate_preflight                    fail-closed decision on Arbitraitor's report
        ├─ platform != linux   → Err(UnsupportedPlatform)   (ADR-0024; no non-secure mode)
        ├─ any control Unavailable → Err(UnavailableControls { missing })
        │                          naming every missing control
        └─ landlock_abi_version: None on linux → filesystem_isolation treated as
                                           missing (see the Landlock-absent gap below)
      Degraded controls never refuse; they are recorded in
      WorkerPreflight::degraded_controls for the run state.
MediatedWorker::run_bash(script)
  └─ arbitraitor_exec ExecutionContextBuilder (via ScriptExecution::bash) with an
     explicit ExecutionPolicy { network_policy: NetworkPolicy::Denied, ..default() }
      → /bin/bash --noprofile --norc over stdin, network namespace, Landlock
        (where the kernel delivers it), no_new_privs, fd closure, fenced
        resource limits; no seccomp filter is installed on this path at the
        pinned revision (syscall_filtering is classification, not enforcement)
```

## Fail-closed semantics

- `probe_worker_preflight` never fails: it records the Arbitraitor
  effective-controls matrix and the derived verdict (`Allowed` / `Refused`)
  into `WorkerPreflight`, the value a caller persists in the run state
  (controls matrix + verdict, per issue #311).
- `gate_preflight` is the only constructor gate for `MediatedWorker`. A
  refusal produces no execution surface at all — no child process, no
  temporary execution directory — so absence of side effects is structural,
  not error-observed (spec `50-contracts-data.md` §21.4).
- Non-Linux platforms get a typed `MediationError::UnsupportedPlatform`. The
  bootstrap loop is Linux-only per ADR-0024; there is deliberately no
  explicitly labelled non-secure mode on this path.
- Error text is log-safe: refusal errors name the missing control identifiers
  and platform; execution failures carry static reason codes translated from
  Arbitraitor `ExecError` variants, never command lines, arguments,
  environment values, or captured child output (spec
  `40-arbitraitor-integration.md` §9.23.4).
- Bash script failures (non-zero exit) are script results, not mediation
  refusals: `MediatedRun` carries the real exit code and captured output.
- Wrapper-start failures are script results, not policy refusals, on the
  worker bash pipeline (`BashChild`, the `unshare` spawn this crate assembles
  itself): a wrapper that dies before the interpreter starts yields `Ok` with
  the wrapper's non-zero exit code and its diagnostic in `stderr` — the script
  did **not** run, and no isolation-off fallback exists on the worker path.
- Namespace-isolation refusals **are** typed refusals on the
  `MediatedWorker::run_bash` path (`ScriptExecution::execute`), which always
  requests a network namespace. Since the pinned revision `df66f2d4` that call
  fails closed with `Err(MediationError::Bash)` and a static reason code
  instead of returning the wrapper's result
  (arbsec/arbitraitor#827): `network-isolation-unavailable` when the wrapper
  refused to set the namespace up (the errno/marker set covers the
  `cannot open /proc/self/uid_map` identity-map shape), and
  `unshare-target-exec-failed` when the namespace existed but the target never
  started. A wrapper failure the classifier does not recognise stays an
  execution failure (`spawn`), never a fabricated refusal.
- Landlock probe consumption (defense in depth; arbsec/arbitraitor#755, tracked
  via #401): at the pinned revision `df66f2d4` the classification is
  probe-coupled — `compute_effective_controls` reports
  `filesystem_isolation: Unavailable` when the host Landlock ABI probe is
  unsupported (upstream commit `f501af7`, ADR-0028), so the matrix and the
  enforcement hook can no longer diverge. The verdict additionally consumes
  the probe datum itself: `landlock_abi_version: None` on linux records
  `filesystem_isolation` as missing and the gate refuses. That check is kept
  as fail-closed defense in depth against a future regression in the coupling;
  on the pinned revision it always coincides with upstream's `Unavailable`. No
  control state is inferred beyond what Arbitraitor reported.
- Degraded controls are recorded, not refused: a
  `ControlState::Degraded` entry lands in `WorkerPreflight::degraded_controls`
  (spec `40-arbitraitor-integration.md` §6.7), mirroring the daemon
  capability report's distinct `degraded` status. The pinned Arbitraitor
  revision never emits `Degraded` today; the field exists so run-state
  records already carry it when upstream starts reporting it.

## Spec-narrative mapping (pinned revision `df66f2d4`)

Spec §9.6 names `configure_command` / `apply_sandbox` conceptually. At the
pinned Arbitraitor revision the real surface is:

- `arbitraitor_sandbox::compute_effective_controls(mode, platform)` —
  preflight probe (existing path `crates/arbitraitor-sandbox/src/lib.rs:409`).
- `arbitraitor_sandbox::configure_command` and
  `arbitraitor_sandbox::configure_filesystem_isolation` — applied inside
  `arbitraitor-exec`'s mediated command construction; the
  `ExecutionContextBuilder` methods are `new`/`from_operation`, `command`,
  `arguments`, `policy`, `source_environment`, `build`
  (`crates/arbitraitor-exec/src/lib.rs:1252`). The builder has no
  `configure_command`/`apply_sandbox` methods; no invented names are used.

## Recorded deferrals and ownership

- The exec-side receipt-matrix probe — spec
  `40-arbitraitor-integration.md` §9.6's second authoritative probe,
  realized upstream as `ExecutionContextBuilder::from_operation` — is
  deferred to E5/`Contained` assurance. At `Mediated` assurance the pinned
  revision defaults the exec effective-controls matrix
  (`crates/arbitraitor-exec/src/lib.rs:1426-1428`), so there is no real
  exec matrix for Orchestraitor to consume on this path.
- Run-state wiring of the preflight record (controls matrix + verdict) and
  any `orc doctor` surface for it are owned by the consumer task #310; this
  task ships the probe/gate/record value only.

## Landlock and the network wrapper's identity map (pinned revision `df66f2d4`)

The `unshare --map-current-user --net` wrapper writes its own user-namespace
identity map through `/proc/self/uid_map`, `/proc/self/setgroups`, and
`/proc/self/gid_map` before it execs the interpreter, so a Landlock ruleset
that withholds those writes stops the mediated run before the script starts.
The pinned Arbitraitor revision closes that hole for its own exec path:
`ScriptExecution` adds exactly three per-file, write-only rules for those paths
whenever network isolation is enabled (`landlock_rules_for_script_execution`,
`crates/arbitraitor-exec/src/script.rs`; upstream #754, fixed in
[arbsec/arbitraitor#780](https://github.com/arbsec/arbitraitor/pull/780)).
`MediatedWorker::run_bash` therefore runs on Landlock-active hosts; a host
whose LSM or policy still refuses those writes surfaces as a classified setup
refusal — `Err(MediationError::Bash)` with reason code
`network-isolation-unavailable` — not as a result (see the fail-closed bullet
above).

The separately assembled `BashChild` path (the worker bash pipeline) builds its
own command and its own rule list
(`crates/orchestraitor-arb-client/src/bash_child.rs`, `landlock_rules`), which
mirrors the pinned rule set *without* those three `/proc` grants, and it does
not classify the wrapper's stderr: a wrapper that dies before the interpreter
starts there yields `Ok` with the wrapper's non-zero exit code and its
diagnostic in `stderr`, as recorded above. Orchestraitor does not widen that
rule list itself — the write-only grant and its non-leverageability argument
belong to Arbitraitor, which owns the wrapper composition
([arbsec/arbitraitor#754](https://github.com/arbsec/arbitraitor/issues/754);
the grant covers upstream's own exec path only).

The live-enforcement tests in the mediation module gate on a full-stack
mediated smoke (`true` through the real path) and skip on hosts where that
smoke does not complete instead of weakening the enforcement posture; the skip
convention is `mediated_stack_or_skip`, whose doc comment documents the
identity-map condition. Missing-control refusals are host-independent and
always run. Orchestraitor-side tracking for the wrapper composition lives in
issue [#400](https://github.com/arbsec/orchestraitor/issues/400).

## Landlock-absent classification (fixed upstream; consumer check kept as defense in depth)

On Linux hosts **without** a working Landlock probe (kernel < 5.13 or LSM
disabled), Arbitraitor before the pinned revision classified
`filesystem_isolation` as `Available` while its enforcement hook installed no
ruleset at all — `install_landlock_ruleset_plan` returns
`LandlockInstallOutcome::NoKernelSupport` when `probe.abi_version()` is `None`
(`crates/arbitraitor-sandbox/src/landlock.rs`). A consumer that trusted the
classification alone would have run mediated workers with **zero filesystem
confinement** while its run-state records claimed isolation — the inverse
direction of the gap above, and more dangerous for containment.

That divergence is fixed at the pinned revision `df66f2d4`:
`compute_effective_controls` couples `filesystem_isolation` to the live ABI
probe and reports `Unavailable` when the probe is unsupported, so "the matrix
and the hook can no longer diverge" (upstream commit `f501af7`, ADR-0028,
[arbsec/arbitraitor#755](https://github.com/arbsec/arbitraitor/issues/755)).

Orchestraitor keeps its own probe-datum check as fail-closed defense in
depth: `landlock_abi_version: None` on the supported Linux platform still
records `filesystem_isolation` as missing and the gate refuses. On the pinned
revision that condition always coincides with upstream's `Unavailable`, so the
check never overrides upstream truth — it only guards against a future
regression in the coupling. No control state is inferred beyond the probe
(spec `40-arbitraitor-integration.md` §16.2).

Disclosure: mediated workers **do not start** on Landlock-absent Linux hosts
(typed `UnavailableControls { filesystem_isolation }` refusal), and the
positive-matrix preflight test adapts to its host's actual probe. Tracking
lives in issue [#401](https://github.com/arbsec/orchestraitor/issues/401)
(upstream `arbsec/arbitraitor#755` is fixed at the pin).
