# Verified-commit path (App-API pushes)

The repo's `main` ruleset enforces `required_signatures`: a commit shows `verified: true`
only when GitHub itself can attest to it. An unsigned locally-created commit carrying the
bot identity in author/committer name+email is **always** unverified — attribution metadata
is not a signature. Verified commits come from exactly two sources:

1. **API-created commits.** Commits created via GraphQL `createCommitOnBranch` by the App
   installation are signed by GitHub (`web-flow` committer) and verify as `reason=valid`.
   The REST `git/commits` endpoint does **not** sign, even with an installation token —
   verified empirically on 2026-10-01 by creating one commit through each API and
   comparing the resulting `verification` payload.
2. **Real GPG/SSH signatures** from a key registered to the committing identity.

## Replay procedure (documented here; no script yet)

The replay script (`scripts/github-app-recreate-branch.sh`) is the designated
home for this procedure but is NOT yet implemented — perform the replay by
following the steps below exactly. Retrofits an existing unsigned commit chain
into GitHub-signed commits:

1. Mint a JWT (RS256, `iss` = App client id, ≤10 min TTL) from the keyring PEM
   (`secret-tool lookup service orchestraitor`), exchange it for an installation token.
2. Walk the branch's commits oldest→newest above the merge-base with `origin/main`.
3. Replay each commit via GraphQL `createCommitOnBranch` on a **temp branch**
   `replay/signed-<branch>`, checking `expectedHeadOid` against the live remote before
   every mutation.
4. Force-swing the real branch ref to the temp head **once**, then delete the temp branch.

**Pitfall:** never reset a PR branch through an ancestor-of-main state mid-replay —
GitHub auto-closes the PR. Replay on the temp branch; swing the ref a single time.

## Security posture (requirements for the future script)

When the replay script is implemented it MUST hold the installation token in
memory only — never echo, log, or persist it; it MUST fail closed on any
error and refuse the Arbitraitor curl shim (which strips Authorization
headers), so the token cannot leak through a proxied request.

## When to use what

- **New agent commits:** create them via the App API (`createCommitOnBranch`) from the
  start — do not push unsigned commits and replay afterwards.
- **Existing unsigned chains:** run the replay script, then verify
  `git rev-parse <old-oid>^{tree}` and `git rev-parse <new-oid>^{tree}` return the
  same tree object ID and record tree identity on the PR (see
  [pr-convergence.md](pr-convergence.md) for why a tree-identical swing preserves
  review convergence).
