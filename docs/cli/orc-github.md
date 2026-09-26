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
