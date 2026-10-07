# `orc github`

`orc github` operates the daemon's GitHub App **service identity**
(spec `10-orchestrator.md` §9.25.2, §9.41). All agent-driven GitHub operations
— issues, PRs, reviews, Projects v2 board writes — authenticate as the App
instead of a personal account. There is no ambient-credential fallback ever:
no `GITHUB_TOKEN` sniffing, no owner-account auth. Registration and permission
details live in the planning runbook `.omo/drafts/github-app-setup.md`.

## Configuration

Layered config keys (spec `50-contracts-data.md` §9.22):

```toml
service_identities = ["arbsec-agent"]   # built-in default

[github_app]
slug = "arbsec-agent"                   # built-in default
client_id = "Iv23linxUDbcc53QbFVK"      # user/org layer; non-secret
installation_id = 165043398             # user/org layer; per-org install
private_key_uri = "secret://keyring/orchestraitor-app-pem"  # or secret://env/<VAR>
```

- `github_app.private_key_uri` follows the spec
  `40-arbitraitor-integration.md` §9.23 resolution: an env URI reads exactly the
  named environment variable; a keyring URI reads the platform keyring entry
  under the `[secrets].keyring_service` label (default `orchestraitor`, behind
  the `secrets-keyring` Cargo feature, enabled by default). Resolution is
  exact-store — if the reference cannot be resolved, minting fails closed.
- The client ID — not the numeric app ID — is the JWT `iss` claim; GitHub
  rejects the app ID form with 401 (runbook §5).
- `service_identities` declares the bot slugs treated as the service identity
  by the ready-queue assignee exclusion (spec `10-orchestrator.md` §9.41):
  items assigned to a human are excluded; items assigned to a service identity
  stay schedulable. The `ready-queue` skill script reads its
  `[service_identities] slugs` list from the project config
  (`.agents/project/github-project.local.toml`), or `$ORC_SERVICE_IDENTITIES`,
  defaulting to `arbsec-agent`.

### Enforcement (`github_app.enforcement`)

The layered config key `github_app.enforcement` controls how strictly the
service identity is enforced when the `github_app` block does not resolve
(absent, partial, or unresolvable). Values:

- `recommended` (default): current behaviour — the skill-script wrapper
  (`orc_lib_gh_service`) takes the labelled personal-auth fallback with a loud
  `WARNING` on stderr. This is a **named bootstrap deviation** (spec
  `10-orchestrator.md` §9.41): the org rule stays "never personal account";
  the fallback is a labelled, temporary bridge until the App identity is
  configured everywhere, never an equal option.
- `required`: fail closed. `orc github gh-env` refuses to delegate (typed
  error naming the missing `github_app.*` keys) — no personal fallback, no
  WARNING; the skill-script wrapper returns its typed config error (exit 2)
  without reaching `gh`. In this mode the wrapper also verifies the repo git
  identity before delegating: `git config user.email` must EQUAL the bot's
  canonical noreply email (`<bot-id>+<slug>[bot]@users.noreply.github.com`),
  resolved live from the App identity (`GET /app` → slug,
  `GET /users/{slug}[bot]` → id) — a suffix match would accept look-alikes
  like `anything+<slug>[bot]@…`, so the comparison is exact. Set the per-repo
  gitconfig to the bot identity (`user.name = arbsec-agent[bot]`,
  `user.email` = the canonical email printed by
  `orc github commit-author`) when operating under `required`.

Invalid values fail closed at parse time (typed configuration error). The
effective value is visible through `orc config get github_app.enforcement`
and `orc config explain github_app.enforcement`. Wrapper-only deployments
(where the skill scripts run without `orc` on `PATH`) may pin the mode with
`ORC_GITHUB_APP_ENFORCEMENT=required`, which takes precedence over the
layered config; when it declares `required`, a missing `orc` binary fails
closed instead of falling back to personal auth. `orc github gh-env` honors
the same pin: an env-pinned `required` runs the full required-mode gate
(complete-config check plus the canonical git-identity check) even when the
layered config does not declare it, and an invalid pin value fails closed.

### Why two `service_identities` surfaces (recorded decision)

The ready-queue skill script runs outside the daemon today and owns its local
config file, so it reads `[service_identities].slugs` from the project config
(or `$ORC_SERVICE_IDENTITIES`) directly rather than querying the layered
configuration. The layered config key `service_identities` is the future
source of truth for the daemon-side scheduling queue. The two surfaces
intentionally mirror the same semantics (case-insensitive `<slug>` or
`<slug>[bot]` matching), and wiring the script to `orc config get
service_identities` is the E0+ integration step — deliberately not part of
the bootstrap-identity change.

## Environment overrides

- `ORCHESTRAITOR_GITHUB_API_ENDPOINT` (hidden, global flag
  `--github-api-endpoint`): overrides the GitHub API base URL used by
  `orc github mint-token`. This is a test/debug override only, for pointing at
  a local stub or mock server. **Security note:** setting it changes the host
  the App JWT bearer is sent to — point it only at infrastructure you control;
  never set it in production or in shared CI against untrusted endpoints.

## `orc github mint-token`

Mints one installation access token: signs an RS256 JWT with the App private
key (`iat = now`, `exp = now + 10min`, `iss = client_id`) and exchanges it via
`POST /app/installations/<installation_id>/access_tokens`. Installed-App
tokens expire after 1h; the daemon-side minter (`orchestraitor-core`
`GitHubAppAuth`) caches them and re-mints at `expiry − 5 minutes`, with a
single-flight guard so concurrent requests never trigger duplicate mints.

```sh
orc github mint-token
```

Output contains only non-secret metadata — the token is never printed, logged,
or persisted:

```text
minted GitHub App installation token
installation_id = 165043398
expires_at = 2026-09-26T12:34:56Z
expires_at_epoch = 1790416496
token_sha256_prefix = 0123abcd4567
token is held in memory only; it is never printed, logged, or persisted
```

Failures exit non-zero with a diagnostic that names the failing layer
(config key, `secret://` reference, transport kind, or HTTP status) and never
contains the PEM, JWT, or token (spec `40-arbitraitor-integration.md` §9.23.4).

## `orc github api`

One authenticated GitHub REST call as the App installation — the service-identity
write path (AGENTS.md "Never operate on GitHub as a personal account"; workflow
policy `.agents/project/orchestraitor-workflow.md`). Mirrors `gh api` minimally:

```sh
orc github api GET /repos/arbsec/orchestraitor/pulls/446
orc github api POST repos/arbsec/orchestraitor/issues/445/comments \
    --field body="Reviewed by arbsec-agent"
orc github api POST /repos/arbsec/orchestraitor/pulls --input body.json
orc github api PATCH /repos/arbsec/orchestraitor/issues/445 --input - <<'JSON'
{"state": "closed"}
JSON
```

Acceptance check for issue #445 (record it as evidence when complete): post a
comment or a review on a REAL pull request as `arbsec-agent[bot]` — an issue
comment does not satisfy it. For example, against an actual PR number:

```sh
orc github api POST repos/arbsec/orchestraitor/issues/<PR-NUMBER>/comments \
    --field body="Identity check: posted by arbsec-agent[bot] via the App installation token."
```

- `METHOD` is one of `GET`, `POST`, `PATCH`, `PUT`, `DELETE`; `PATH` is relative
  to the configured base URL (leading `/` optional).
- The JSON body comes from `--input FILE` (`-` = stdin; not valid with
  GET/DELETE — GitHub ignores request bodies there) and/or repeatable
  `--field key=value` pairs (`value` is ALWAYS a string, like `gh api
  -f/--raw-field` — `-f body=123` sends `"123"`, never the number `123`;
  `-f body={"x":1}` sends the literal text, not an object). On GET/DELETE the
  pairs become percent-encoded URL query parameters (the `gh api` shape);
  otherwise `--field` merges into an `--input` object body, and combining it
  with a non-object body is a typed error.
- The minted installation token is attached as `Authorization: Bearer …` only.
  The response body prints to stdout verbatim: it is the caller's business,
  including on failures. The exit code is 0 for 2xx, non-zero otherwise; the
  non-zero diagnostic carries the HTTP status and endpoint shape — never the
  Authorization header, and never the response body (the body was already
  printed once, on stdout).
- Every diagnostic is token-free. The token lives in memory
  (`secrecy::SecretString`) for the duration of the process and is never
  printed, logged, or persisted.

## `orc github gh-env`

Executes ONE child command with `GH_TOKEN` set to a freshly minted
installation token — the handoff for `gh`-based flows (PR creation, review
threads) that predate the API passthrough:

```sh
orc github gh-env -- gh pr create --draft --title "…" --body "…"
orc github gh-env -- gh pr view 446 --json state
```

- The child's stdout/stderr pass through unchanged and the child's exit code
  propagates (a missing exit code maps to 1).
- In `required` enforcement mode the child's git identity is PINNED via
  `GIT_AUTHOR_NAME`/`GIT_AUTHOR_EMAIL`/`GIT_COMMITTER_NAME`/
  `GIT_COMMITTER_EMAIL` (the canonical bot identity from `commit-author`).
  These environment variables take precedence over repo config AND over
  per-invocation `git -c user.email=…` overrides, so a delegated child cannot
  stamp personal attribution onto commits even if it rewrites its identity.
  Identity values are non-secret (public bot login + noreply email).
- The token never appears in `orc`'s own output: it exists only in the child
  process environment. **Trust model:** the child can read and leak its own
  environment — that is the operator's responsibility, the same model as
  `gh auth token | xargs`. Do not point `gh-env` at commands you do not trust.
- The child command is validated to exist on `PATH` (or as a direct path) AND
  carry the executable bit BEFORE minting; a missing or non-executable command
  is a typed error and no token is minted.

## `orc github commit-author`

Prints ONLY the App's canonical commit identity as two lines, derived from the
authenticated App (`GET /app` → `slug`, then
`GET /users/{slug}[bot]` → the bot user id → the GitHub noreply email
convention) — never hardcoded. `GET /app` is an **App-level endpoint**:
GitHub rejects installation tokens with 401, so this subcommand authenticates
it with a freshly minted **App JWT** (RS256, `iss = client_id`, 10-minute
lifetime) signed from the App private key — the same secret material and the
same secrecy rules as the mint path. The JWT is held in memory only, injected
solely into the one `Authorization` header, and never printed, logged, or
persisted. The bot-user lookup is authenticated with the installation token
for the configured organization's installation (GitHub rejects the App JWT on
`GET /users`, and on an Enterprise Managed Users organization the bot profile
is not publicly visible — an unauthenticated request would answer 404). The
other subcommands (`mint-token`, `api`, `gh-env`) keep using
installation tokens.

```sh
$ orc github commit-author
name=arbsec-agent[bot]
email=334074867+arbsec-agent[bot]@users.noreply.github.com
```

The output above is the contract for a correctly configured App. It requires
live GitHub access with the registered App's private key: the request
authenticates the App itself, not an installation, so a stubbed
installation-token endpoint cannot VERIFY the JWT — verifying the signature
needs the real App. A stub that also serves `GET /app` and
`GET /users/{slug}[bot]` CAN satisfy the CLI test (the CLI only needs
well-formed responses over HTTP); it just proves response handling, not App
identity. Failures (unresolvable private key, unreachable API, HTTP 401 on a
malformed JWT) exit non-zero with a typed diagnostic that never contains the
PEM, the JWT, or any token. Any malformation in the `/app` or
`/users/{slug}[bot]` payload is a typed error; the payload is never echoed.

## `orc github push-branch`

Lands a local branch's tree as ONE App-signed squashed commit on the remote
branch — the agent push path for repositories with `required_signatures`
rulesets, where plain `git push` produces unverified commits that are rejected
at merge time (skill `verified-commit-path.md`; spec `10-orchestrator.md`
§9.41 service identity):

```sh
orc github push-branch --branch feat/my-change \
  --message "feat(widget): add blinking" \
  --body-file body.md \
  --owner arbsec --repo orchestraitor
```

Arguments and defaults:

- `--branch <name>` — the local branch whose HEAD tree is landed. Defaults to
  the current branch of the working directory (detached HEAD is a typed
  error).
- `--remote-branch <name>` — the remote branch to create or force-move.
  Defaults to the local branch name.
- `--owner <owner>` / `--repo <name>` — the target repository. `--repo` also
  accepts the full `owner/name` slug, which is used as-is (a bare name
  combines with the owner resolved from the `--owner` flag or the `origin`
  remote of the current directory
  (`git@github.com:owner/repo.git` or the https shape; anything else is a
  typed error telling you to pass both flags)).
- `--base <ref>` — the diff base for a NEW remote branch. Defaults to the
  repository's default branch, read from the API. For an EXISTING remote
  branch the diff base is always that branch's remote head tree (an earlier
  landing may have moved it past the base; a second landing carries only the
  delta), so `--base` only decides what is fetched for comparison.
- `--message <headline>` (required) and `--body-file <file>` (`-` = stdin) —
  the squashed commit's message.
- `--re-land` — see the re-land section below.
- `--dry-run` — resolve everything (owner/repo, branches, diff base, change
  set) and print the plan (paths and statuses only — never blob contents)
  without creating or moving anything on the remote.

Mechanics:

- Computes the local branch's tree (`git rev-parse <branch>^{tree}`). The
  change set is the diff from the REMOTE HEAD's tree when the branch exists on
  the remote (fetched locally first — an earlier landing may have moved the
  remote tree past the base), or from the base branch tree for a new branch.
  `A`/`M` additions carry the branch's committed blob CONTENTS, base64-encoded
  (`git cat-file blob` — never the working directory, and never blob OIDs
  pasted as contents: that was the corrupted-FileAddition incident this
  subcommand exists to prevent). `D` deletions carry the path. An empty diff
  (the local tree equals the remote head tree) is a no-op — the decision
  needs the remote head, so one minted token and the branch-ref request are
  spent before the refusal.
- **Squash-landing:** a local branch with multiple commits lands as ONE
  remote commit — the landing is a tree operation (local HEAD tree vs. the
  remote head tree), never a history replay. The local branch keeps its own
  history; the remote branch receives exactly one squashed commit.
- **Landing via a temporary branch:** the commit is created on
  `push-branch/tmp-<uuid>` — a fresh, invocation-unique name created with
  `createRef` (`createCommitOnBranch` does NOT auto-create branches),
  bootstrapped at the observed remote head when the branch exists, or at the
  base commit for a new branch. This puts the tree/verification gates BEFORE
  the real ref moves — `createCommitOnBranch` advances whatever branch it
  lands on, so landing directly would move the real branch even when a gate
  later fails. The temp ref is deleted on every path, and only the ref id
  this invocation created is ever touched.
- **Gates (fail-closed, typed errors, no plain-`git push` fallback):** the
  landed commit's tree MUST equal the local branch tree, and its
  `signature.isValid` MUST be `true` (GitHub web-flow signing). A failed gate
  deletes the temp ref and leaves the real remote branch untouched.
  Diagnostics never contain the token.
- **Ref move with an exact-head precondition AND a false-negative guard:**
  the verified landing commit was created ON TOP of the observed remote head;
  the real ref is moved with the `updateRefs` mutation carrying
  `RefUpdate.beforeOid = <observed head>` and `force = false`. `beforeOid` is
  an exact precondition: a concurrent writer that ADVANCES the branch in the
  window makes the update non-fast-forward, and one that REWINDS it makes
  `beforeOid` mismatch — both FAIL with a typed error, and the concurrent
  commit is never overwritten in either direction. A new remote branch is
  created directly at the verified commit. Additionally, the
  `updateRefs`/`clientMutationId` response is KNOWN to be flaky: GitHub has
  answered with a malformed-payload/GraphQL error envelope AFTER the mutation
  actually applied. Whenever an updateRefs error or a missing
  `clientMutationId` occurs, the subcommand re-reads the branch ref through
  the API before failing: a ref that provably points at the new head
  degrades the failure to a WARNING on stderr and reports success — a
  mutation that moved the ref is never reported as an error.
- **Executable-file mode limitation:** `createCommitOnBranch` `FileAddition`
  always lands files as 100644 — it cannot set the executable bit. When a
  file being ADDED is 100755 in the local tree (and was not already 100755 in
  the diff base), the subcommand prints a WARNING on stderr before landing:
  the remote tree will carry 100644 and the tree gate will then refuse the
  landing. Fix by committing the file as 100644 locally, or land it first
  with mode 100644 and chmod locally afterwards. Files that already exist in
  the diff base with 100755 are unaffected (modifications keep GitHub's
  stored mode).
- In `required` enforcement mode the complete-`github_app` gate applies before
  any network call.

### Re-landing a rebased branch (`--re-land`)

After a conflict resolution rebase, the local branch's tree is usually already
identical to the remote head (the merge was content-free) but the PR head
chain carries commits `required_signatures` cannot verify (a plain-push
history, or any unsigned ancestor). `push-branch --re-land` covers exactly
this case:

- lands ONE empty App-signed commit whose parent is the current remote head —
  the PR head moves to a verified commit (the new head commit only; unsigned
  ancestors are not rewritten — see the limitation below);
- LIMITATION: this verifies the HEAD commit; it does not rewrite unsigned
  ancestors into verified objects — a ruleset that range-checks every commit
  can still block the merge (range repair needs the manual replay in
  `references/verified-commit-path.md`);
- is a no-op (exit 0, no mutation) when the remote head is already verified;
- fails closed (typed error) if the landed empty commit is not verified —
  the real ref never moves in that case (the landing went to the temporary
  branch and the gate fires before the `updateRefs` swing), so no manual
  restore is needed.

Without `--re-land`, an identical-tree landing stays the safe no-op it has
always been.

## `orc github verify-identity`

Verifies that the ambient git commit identity of a checkout matches the
App-derived canonical commit identity — the Rust twin of the
`commit-identity` skill script (agents run the shell script pre-commit; orc
uses the native check in its own flows):

```sh
orc github verify-identity            # current directory
orc github verify-identity ../orchestraitor-my-slice   # explicit path
```

- Compares `git config user.name` and `user.email` exactly as a commit in
  PATH would resolve them (repo-local → global precedence, git's normal
  rules) against the service-identity pair printed by
  `orc github commit-author` — derived live from the App, never hardcoded.
- Exit 0 prints `identity OK: <name> <<email>>` on stderr. On mismatch the
  typed error names the ambient identity, the expected identity, and BOTH
  exact fixes:
  `git -c user.name='…' -c user.email='…' commit …` (per-command) and
  `git config user.name '…' && git config user.email '…'` (per-checkout).
- An UNSET `user.name`/`user.email` is also a typed error: git would fall
  back to an auto-detected identity, which is exactly how a fresh worktree
  inherited the global personal gitconfig and stamped the human owner's
  identity onto PR #486's commits.
- Run it before any local `git commit` destined for a PR branch — or let
  `orc github gh-env` pin the identity for you in `required` mode (it sets
  `GIT_AUTHOR_*`/`GIT_COMMITTER_*` in the child environment).

## Rollback

Removing the `github_app` configuration (or unsetting any of `client_id`,
`installation_id`, `private_key_uri`) makes all six subcommands fail closed
with a typed `github_app.*` configuration error — the commands mint nothing and
fall back to nothing. Roll back the binary by reverting this change; there is
no persisted state to clean up.
