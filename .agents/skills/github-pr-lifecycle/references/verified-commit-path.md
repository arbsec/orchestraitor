# Verified-commit path (App-API pushes)

The repo's `main` ruleset enforces `required_signatures`: a commit shows `verified: true`
only when GitHub itself can attest to it. An unsigned locally-created commit carrying the
bot identity in author/committer name+email is **always** unverified — attribution metadata
is not a signature. Verified commits come from exactly two sources:

1. **API-created commits.** Commits created by the App installation are signed by GitHub
   and verify as `reason=valid`. Both transports do this:

   - **GraphQL `createCommitOnBranch`** — signs unconditionally (`web-flow` committer).
     This is the default for everyday landings (`orc github push-branch`).
   - **REST `git/commits`** — signs too, but ONLY when the request body omits `author`,
     `committer` AND `signature`. Supplying `author`+`committer` makes GitHub store them
     verbatim and return `reason=unsigned` with no signature: signing is suppressed by
     the identity fields, not by the endpoint.

   Probed 2026-11-09 against `arbsec/orchestraitor` with an App installation token
   (throwaway refs, since deleted), POSTing the same tree, message and parent:

   | Request body | Commit | `verification` |
   | --- | --- | --- |
   | `message` + `tree` + `parents` only | `e1723feec88e42e24312f7998e8bad1f8b591f65` | `verified = true`, `reason = valid`, PGP signature present |
   | plus explicit `author[…]` and `committer[…]` | `343c4b9534b5a21b3e22b9d828c198f4122eb7ff` | `verified = false`, `reason = unsigned`, no signature |

   In the signed case GitHub rewrote the committer to `GitHub <noreply@github.com>` and
   auto-attributed the author to `arbsec-agent[bot]`. **Scope of the observation:** this
   repository, this App installation, and the `author`+`committer` pair only. A PARTIAL
   identity (author only, or committer only) was not probed — treat any identity field as
   verification-dropping until it is. Supplying `signature` was likewise not exercised:
   both probe bodies omitted it, and GitHub documents the field as verify-as-supplied (a
   supplied signature is not replaced by GitHub's own), so the documented rule stays "omit
   all three" — that is a scope limit, not a measured behaviour.
2. **Real GPG/SSH signatures** from a key registered to the committing identity.

## Replay procedure (documented here; no script yet)

The replay script (`scripts/github-app-recreate-branch.sh`) is the designated
home for this procedure but is NOT yet implemented — perform the replay by
following the steps below exactly. Retrofits an existing unsigned commit chain
into GitHub-signed commits:

1. Mint a JWT (RS256, `iss` = App client id, ≤10 min TTL) from the keyring PEM
   (`secret-tool lookup service orchestraitor`), exchange it for an installation token.
2. Derive the replay base from the PR itself — the PR's base ref and its base
   OID (GraphQL `pullRequest.baseRefName` / `baseRefOid`), not a hardcoded
   branch name. Walk the branch's commits oldest→newest above that base OID.
   Concrete default-branch restrictions are project policy
   (`.agents/project/orchestraitor-workflow.md`), not part of this mechanism.
3. Replay each commit via GraphQL `createCommitOnBranch` on a **temp branch**
   `replay/signed-<branch>`, checking `expectedHeadOid` against the live remote before
   every mutation.
4. Force-swing the real branch ref to the temp head **once**, as a
   compare-and-swap: the ref update MUST carry the latest observed branch OID
   as its expected value (GraphQL `updateRef` with `expectedHeadOid`, fetched
   immediately before the mutation) and the swing MUST abort on mismatch — a
   concurrent commit in the window MUST NOT be overwritten. Delete the temp
   branch only after the swing succeeds.

**Pitfall:** never reset a PR branch through an ancestor-of-main state mid-replay —
GitHub auto-closes the PR. Replay on the temp branch; swing the ref a single time.

## Security posture (requirements for the future script)

When the replay script is implemented it MUST hold the installation token in
memory only — never echo, log, or persist it; it MUST fail closed on any
error and refuse the Arbitraitor curl shim (which strips Authorization
headers), so the token cannot leak through a proxied request.

## When to use what

- **New agent commits:** land them via `orc github push-branch` (the
  App-signed landing subcommand — it wraps `createCommitOnBranch` + the ref
  swing with tree-equality and `verified = true` gates) from the start — do
  not push unsigned commits and replay afterwards.
- **Existing unsigned chains:** run the replay script, then verify
  `git rev-parse <old-oid>^{tree}` and `git rev-parse <new-oid>^{tree}` return the
  same tree object ID and record tree identity on the PR (see
  [pr-convergence.md](pr-convergence.md) for why a tree-identical swing preserves
  review convergence).
- **Executable-bit repairs (#556):** `createCommitOnBranch` cannot express `100755` —
  use the REST tree + commit transport below.

## REST tree + commit (the file-mode transport)

`createCommitOnBranch` `FileAddition` carries no file-mode field, so an executable-bit
repair (#556) is unrepresentable in the RAW mutation: an added blob always lands
`100644`, which the tree-equality gate then refuses (`docs/cli/orc-github.md` documents the
same limitation of the raw mutation). Since #556 `orc github push-branch` routes every
mode-requiring change set through the REST tree + commit transport below automatically,
so it lands an added `100755` file (or a mode change in either direction) with the exact
local mode. The procedure below stays the way to land a HAND-BUILT payload. The REST tree
+ commit path CAN express the mode and still land a verified commit:

1. `POST /git/trees` accepts an entry with `mode: "100755"` (a string, not a number) and
   returns it as `{"path": …, "mode": "100755", "type": "blob"}` (probed 2026-11-09, same
   repository and installation token as the table above).
2. `POST /git/commits` with `message`, `tree` and `parents` and **no** `author`,
   `committer` or `signature` returns the GitHub-signed commit from that table
   (`verified = true`, `reason = valid`).
3. Move the ref with GraphQL `updateRef` carrying `expectedHeadOid` — the exact-head
   precondition the replay procedure uses; REST `PATCH /git/refs/{ref}` with `force: false`
   guards only against a non-fast-forward, so it cannot express that precondition (a
   concurrent rewind of the branch would slip through).

The constraint is the whole trick: identity fields MUST be omitted. Adding `author` and
`committer` for a nicer attribution silently drops verification (`reason = unsigned`) —
the commit is created successfully, so the failure only surfaces later as a
`required_signatures` block. Never add them "just to attribute the bot".
