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

- `METHOD` is one of `GET`, `POST`, `PATCH`, `PUT`, `DELETE`; `PATH` is relative
  to the configured base URL (leading `/` optional).
- The JSON body comes from `--input FILE` (`-` = stdin) and/or repeatable
  `--field key=value` pairs (`value` parses as JSON when valid — `true`,
  `42` — else is a string, like `gh api -f`). `--field` merges into an
  `--input` object body; combining it with a non-object body is a typed error.
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
- The token never appears in `orc`'s own output: it exists only in the child
  process environment. **Trust model:** the child can read and leak its own
  environment — that is the operator's responsibility, the same model as
  `gh auth token | xargs`. Do not point `gh-env` at commands you do not trust.
- The child command is validated to exist on `PATH` (or as a direct path)
  BEFORE minting; a missing command is a typed error and no token is minted.

## `orc github commit-author`

Prints ONLY the App's canonical commit identity as two lines, derived from the
authenticated App (`GET /app` → `slug` + `bot.id` → the GitHub noreply email
convention) — never hardcoded:

```sh
$ orc github commit-author
name=arbsec-agent[bot]
email=334074867+arbsec-agent[bot]@users.noreply.github.com
```

Use the pair for commit flows that must attribute authorship to the service
identity (workflow policy: bot-authored commits + DCO sign-off). Any
malformation in the `/app` payload is a typed error; the payload is never
echoed.

## Rollback

Removing the `github_app` configuration (or unsetting any of `client_id`,
`installation_id`, `private_key_uri`) makes all four subcommands fail closed
with a typed `github_app.*` configuration error — the commands mint nothing and
fall back to nothing. Roll back the binary by reverting this change; there is
no persisted state to clean up.
