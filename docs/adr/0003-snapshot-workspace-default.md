# ADR 0003: Snapshot workspace as the default mode

**Status:** Accepted
**Date:** 2026-07-30
**Issue:** #69

## Context

Spec §9.4 defines four workspace modes with different security profiles. The
choice of default determines how much Git metadata and host state an untrusted
worker can reach. A worktree is not a sandbox (spec §6.2) — the trusted
controller owns Git metadata — but the workspace mode controls how much of
that metadata is materialized into the worker's filesystem view.

The agent is always untrusted (spec §6.1): model, wrapped harness, repository
content, tools, MCP servers, skills, and generated artifacts may behave
incorrectly or maliciously. The default workspace mode must therefore minimize
the worker's access to trusted state without requiring explicit user opt-in.

## Decision

**Snapshot mode (spec §9.4 mode 1) is the default workspace mode.**

### How it works

- The controller uses `gix` 0.85.0 to export a commit tree into disposable
  storage (tech-stack §6.3).
- The worker receives a worktree **without `.git/`**. There is no `.git`
  directory, no Git object database, no refs, no hooks, no config — nothing
  the worker can read, mutate, or use to escape the workspace.
- Any history access the worker needs MUST go through typed RPC methods backed
  by `gix`, never through filesystem `.git/` access.
- The controller imports a patch after inspection (spec §9.4 mode 1, §9.5
  transaction engine, §9.14 output quarantine).

### `gix` with `bail_if_untrusted`

The controller opens repositories with `gix::open_opts()` combined with
`bail_if_untrusted(true)` (tech-stack §6.1). This refuses to execute
user-level `core.fsmonitor`, hooks, or other untrusted filesystem side
effects that a malicious repository could plant. This is the safer choice for
controller-owned Git metadata isolation.

`git2` (libgit2) is rejected because of the C dependency surface and the
patched OpenSSL path it pulls in on Linux.

### Why snapshot is the strongest security default

Of the four modes:

| Mode | Worker sees `.git/`? | Git actions | Policy |
|---|---|---|---|
| **Snapshot (default)** | No | Via typed RPC only | Default |
| Brokered worktree | Files only, not unrestricted metadata | Through controller | Weakened |
| Full worktree | Yes | Direct | Explicit weakened-policy selection |
| Host | User's current checkout | Direct | Explicit override only |

Snapshot mode gives the worker the least access to trusted state. The worker
cannot read Git config, cannot mutate refs, cannot trigger hooks, and cannot
discover commit history beyond what the controller explicitly exposes via RPC.

### Backend selection

Snapshot mode corresponds to the `materialized` workspace projection backend
(spec §9.4.2, tech-stack §6.2). Backend selection is Arbitraitor's decision
based on capability reports: the daemon calls
`arbitraitor_sandbox::compute_effective_controls()` plus a projection-specific
capability probe. If the strongest backend supported by the platform fails the
conformance test, Orchestraitor MUST NOT silently fall back — it reports the
failure, the selected weaker backend, the unsupported semantics, and the
resulting enforcement level.

**Upstream prerequisite:** if Arbitraitor does not yet expose a
workspace-projection API, Orchestraitor uses the `materialized` backend (the
snapshot mode) and MUST NOT claim per-operation mediation or live attribute
enforcement (tech-stack §6.2).

## Consequences

- Untrusted workers cannot reach Git metadata by default. Repository-level
  attacks (malicious hooks, poisoned `core.fsmonitor`, crafted refs) are
  neutralized because the worker has no `.git/` to interact with.
- History operations require an explicit controller-mediated RPC, giving the
  controller an audit point and policy gate.
- Users who need brokered or full worktree mode must make an explicit,
  weakened-policy selection — the default never silently grants more access.
- The `materialized` backend has no per-operation VFS mediation; mutation
  attribution relies on the §9.5 transaction engine + §9.14 output promotion,
  not on a per-operation VFS layer. This is documented, not hidden.

## Alternatives considered

- **Brokered worktree as default:** Rejected. The worker sees files and can
  attempt Git actions through the controller, but the shared Git metadata is
  closer to the worker. Snapshot mode provides a stronger default isolation.
- **Full worktree as default:** Rejected. The worker can access Git metadata
  directly, requiring explicit weakened-policy selection. Not appropriate as a
  default for untrusted agents.
- **Host mode as default:** Rejected. The agent runs in the user's current
  checkout with no isolation. Explicit override only (spec §9.4 mode 4).
- **`git2` (libgit2) instead of `gix`:** Rejected. C dependency surface and
  patched OpenSSL path on Linux. `gix` is pure Rust with `bail_if_untrusted`
  for controller-owned isolation.

## References

- `docs/spec/spec.md` §6.1 (The agent is always untrusted)
- `docs/spec/spec.md` §6.2 (A worktree is not a sandbox)
- `docs/spec/spec.md` §9.4 (Workspace and Git controller)
- `docs/spec/spec.md` §9.4.2 (Arbitraitor-managed workspace projection)
- `docs/spec/spec.md` §9.5 (Transaction engine)
- `docs/spec/spec.md` §9.14 (Output quarantine)
- `docs/spec/tech-stack.md` §6.1 (Library choice — `gix`)
- `docs/spec/tech-stack.md` §6.2 (Arbitraitor-managed workspace projection)
- `docs/spec/tech-stack.md` §6.3 (Snapshot workspace mode)
