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
       └─ any control Unavailable → Err(UnavailableControls { missing })
                                           naming every missing control
MediatedWorker::run_bash(script)
  └─ arbitraitor_exec ExecutionContextBuilder (via ScriptExecution::bash) with an
     explicit ExecutionPolicy { network_policy: NetworkPolicy::Denied, ..default() }
     → /bin/bash --noprofile --norc over stdin, network namespace, Landlock,
       no_new_privs, fd closure, fenced resource limits
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

## Spec-narrative mapping (pinned revision `4ebebb3`)

Spec §9.6 names `configure_command` / `apply_sandbox` conceptually. At the
pinned Arbitraitor revision the real surface is:

- `arbitraitor_sandbox::compute_effective_controls(mode, platform)` —
  preflight probe (existing path `crates/arbitraitor-sandbox/src/lib.rs:387`).
- `arbitraitor_sandbox::configure_command` and
  `arbitraitor_sandbox::configure_filesystem_isolation` — applied inside
  `arbitraitor-exec`'s mediated command construction; the
  `ExecutionContextBuilder` methods are `new`/`from_operation`, `command`,
  `arguments`, `policy`, `source_environment`, `build`
  (`crates/arbitraitor-exec/src/lib.rs:1104-1228`). The builder has no
  `configure_command`/`apply_sandbox` methods; no invented names are used.

## Known upstream gap (recorded, owned by Arbitraitor)

On hosts with the Landlock LSM active, the pinned revision applies its
Landlock ruleset to the `unshare` network-namespace wrapper process, which
then cannot open `/proc/self/uid_map` and the mediated child dies with
`unshare: cannot open /proc/self/uid_map: Permission denied`. Upstream's own
happy-path exec tests run with network isolation disabled, so only a
full-stack smoke run exposes this. The live-enforcement tests in the
mediation module gate on a full-stack mediated smoke (`true` through the real
path) and skip on affected hosts instead of weakening the enforcement
posture; missing-control refusals are host-independent and always run. The
fix belongs in `arbsec/arbitraitor` (Landlock rules vs. wrapper composition),
not in Orchestraitor (spec `40-arbitraitor-integration.md` §16.2).
