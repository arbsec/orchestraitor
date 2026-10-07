# Commit-hygiene remediation (failure-class → detection → remediation)

Agent commits are created through GitHub's App API (`createCommitOnBranch`,
landed by `orc github push-branch`), which bypasses every local hook — the
lefthook chain (`cog verify`, `check-unsigned-bot-commits`) never sees them.
Post-hoc enforcement is therefore an audit, not a hook:
`scripts/audit-pr-commits <pr>` runs in the PR loop (SKILL.md step 2) and
classifies each PR commit (base..head) into the failure classes below. Every
class is a REAL failure (policy exit 3): fix the root cause — never reroll a
commit to make the audit green, never force-push an unsigned chain, never
disable the audit.

The remediation for the first three classes is always the same mechanism —
recreate the offending commit(s) via the verified-commit path — with
class-specific deltas. The verified-commit path is documented in
[verified-commit-path.md](verified-commit-path.md): replay the branch's
commits oldest→newest via GraphQL `createCommitOnBranch` on a temp branch
`replay/signed-<branch>` (each mutation carries `expectedHeadOid`, fetched
immediately before the call), then swing the real branch ref to the temp head
exactly ONCE as a compare-and-swap `updateRef`. New commits never go through
the replay path at all: land them with `orc github push-branch` from the
start (correct author derivation comes from `orc github commit-author` — the
canonical `name=<slug>[bot]` line — never from ambient git config).

## unauthorized-identity

- **Detection**: `audit-pr-commits` class `unauthorized-identity`. The
  compare API returns `.author = null` for an email that resolves to no
  GitHub account (reported as `NO_USER`), or `.author.login` outside the
  declared principal set (`arbsec-agent[bot]`, `renovate[bot]`). The two
  signature cases:
  - an agent committing as the human owner (`mekwall`) — identity misuse;
  - a commit whose author email resolves to nothing (`NO_USER`) — an
    identity the ruleset cannot attribute.
- **Why it happens**: ambient git config leaked into the commit authorship
  (`git commit` run outside the service wrapper), or a hand-authored
  name/email pair that matches no account.
- **Remediation**: recreate the commit via the verified-commit path with the
  author/committer derived from `orc github commit-author` (the App bot
  identity) instead of whatever ambient identity produced the bad commit.
  Tree content is unchanged; only authorship metadata is re-derived. If the
  commit was authored as the human owner, treat it as an identity-misuse
  incident: fix the environment that produced it (the wrapper's
  enforcement=required check exists precisely to fail this closed) and note
  the correction on the PR.
- **Do NOT**: "fix" attribution by amending locally and force-pushing — an
  unsigned bot-identity commit never verifies (attribution metadata is not a
  signature; the `required_signatures` ruleset blocks it at merge).

## unverified-commit

- **Detection**: `audit-pr-commits` class `unverified-commit`;
  `.commit.verification.verified != true` (reason e.g. `unsigned`,
  `unknown_key`, `expired_key`).
- **Why it happens**: the commit was created outside the App API — a plain
  `git push` (unsigned; locally-attributed bot commits can never verify), or
  a REST `git/commits` creation (REST does not sign even with an installation
  token — verified empirically 2026-10-01), or signed with a key GitHub does
  not recognize (`unknown_key`: e.g. a committer key not registered to the
  author identity, as seen on PR #486's renovate-authored commit).
- **Remediation**: there is NO local way to make such a commit verify —
  unsigned commits cannot be signed after the fact, and a local GPG/SSH
  signature under the bot identity is not a registered path. Recreate the
  commit through `createCommitOnBranch` (App-signed, `web-flow` committer,
  `reason=valid`) via the verified-commit path — or, for brand-new work,
  land it with `orc github push-branch` from the start. The only alternative
  verification source is a real GPG/SSH signature from a key registered to
  the committing identity; service principals have none, so the App-API path
  is the only one for agent commits.
- **Do NOT**: push the unsigned commit and hope; merge with
  `required_signatures` off; use admin bypass (workflow policy forbids both
  unconditionally).

## non-conventional-message

- **Detection**: `audit-pr-commits` class `non-conventional-message`; the
  subject line does not match the repo's Conventional Commits grammar
  (types: `feat`, `fix`, `security`, `docs`, `refactor`, `test`, `ci`,
  `chore`, `build`, `perf`, `revert`; optional scope in parentheses; optional
  `!`; then a colon-space and a non-empty subject — the same shape
  `cog verify` enforces locally).
- **Why it happens**: a commit written outside the wrapper discipline
  (e.g. a bare sentence subject), or a tool-generated message that skips the
  format.
- **Remediation**: recreate the commit via the verified-commit path with a
  corrected Conventional Commits subject (tree unchanged — a message-only
  fix). Remediation landings keep the same rule: each carries its OWN
  conventional message naming what it fixes, never a duplicate of the PR
  headline.

## Mode/permission anomalies (createCommitOnBranch)

- **Anomaly**: `createCommitOnBranch` `FileAddition` forces mode `100644` —
  a file that was `100755` (executable) in the base tree lands as
  non-executable after an App-API replay (experienced the week of
  2026-10-06). There is no `mode` field to set on the mutation.
- **Detection**: `git diff --summary <base>..<head>` shows mode changes not
  present in the intended diff (`mode change 100755 => 100644`), or a
  script/hook silently stops being executable after a replay.
- **Remediation**: re-land the affected file via `orc github push-branch`
  (which derives the tree from the local branch and is not subject to the
  constraint the way a hand-built FileAddition payload is), or push a
  follow-up App-API commit whose tree restores `100755` — verify the mode in
  the final tree, not in the working directory. Design replay payloads so
  executability survives: if the tree is built from the real base tree
  object (not per-file FileAdditions), modes are preserved.

## GitHub ghost-commit anomaly

- **Anomaly**: during a replay, the compare/ref view has shown a commit that
  exists in neither the local branch nor the replay payload — a ghost commit
  attributed to the ref swing window (experienced the week of 2026-10-06).
  It appears in `compare` output but not in any branch tip computation done
  locally.
- **Detection**: `audit-pr-commits` flags a commit nobody can account for
  (`NO_USER` authorship is common for these), or the PR's commit count
  exceeds the locally-known chain length by exactly one immediately after a
  ref swing.
- **Remediation**: never "fix" a ghost by resetting the PR branch to an
  ancestor of `main` mid-replay — GitHub auto-closes the PR on that state.
  Re-run `audit-pr-commits` after the ref settles; if the ghost persists,
  re-land the branch from the known-good local tree with `orc github
  push-branch --re-land` (single CAS swing) so the ref points at a fully
  accounted chain, and verify tree identity
  (`git rev-parse <old-oid>^{tree}` == `git rev-parse <new-oid>^{tree}`) so
  review convergence is preserved.

## What NOT to do (all classes)

- **No force-pushing unsigned local commits** — they can never verify
  (`required_signatures` blocks them at merge; attribution is not a
  signature).
- **No resetting a PR branch to an ancestor of `main` mid-replay** — GitHub
  auto-closes the PR. Replay on a temp branch; swing the ref once, CAS.
- **No admin bypasses** — no `--admin` merges over a red gate, no ruleset
  disables, no rerolling the audit until it passes. Violations are fixed by
  recreating commits correctly, not by lowering the gate.
- **No personal-account GitHub writes** — the owner mandate (2026-10-02)
  forbids `@mekwall` writes on this repository entirely; the fix identity is
  always the App service identity.
