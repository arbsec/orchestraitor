# `orc board`

`orc board` reads and updates the shared GitHub Projects v2 board used for MVP delivery
(spec `10-orchestrator.md` §9.43, §9.40). It is the bootstrap slice of the board-provider
contract: a ready-queue read and a verified Status write, nothing more.

## Configuration

The board is described by human-readable NAMES in
`.agents/project/github-project.local.toml` (copy the committed
`.agents/project/github-project.example.toml`; the example is documentation only and is never
loaded). The command searches upward from `--project-dir` for the local file, so it works from
any directory inside the checkout. GitHub node IDs are never stored in config or committed:
organization/project/field/option node IDs are resolved at runtime via GraphQL and cached in
`$XDG_CACHE_HOME/orchestraitor/gh-project-fields.json` (or `~/.cache/orchestraitor/`), keyed by
organization and project number (spec §9.43).

Authentication is explicit — no ambient credential sniffing. Until the GitHub App service
identity module lands, set a bootstrap token URI in the local config:

```toml
[auth]
token = "secret://env/GH_TOKEN" # any env var name; `secret://keyring/<id>` is
                                # a typed "unavailable" error in the bootstrap slice
```

The token is held in a `secrecy::SecretString` (spec
`40-arbitraitor-integration.md` §9.23), injected only into the
`Authorization` header of GraphQL requests, and never appears in errors, logs, or output.

## Commands

```sh
orc board ready [--json]
orc board move <issue-number> --status "<Status option name>"
```

- `orc board ready` lists items on the shared board that are leaf Task/Bug (native issue type
  wins over labels; the lowercase `task`/`bug` labels are the fallback while org-level types
  are unset), `Target=MVP`, `Status=Ready`, and have no unresolved `blockedBy` edges (open
  blockers and truncated blocker windows both exclude). Only the repositories listed under
  `[project].repos` are in scope, and results sort by issue number. Malformed or undecidable
  items are skipped with a warning on stderr, never a crash.
- `orc board move` finds the board item for an issue number in the configured repositories,
  writes the Status single-select field via `updateProjectV2ItemFieldValue`, and verifies the
  write by reading the field back before reporting success.

`--json` on `ready` emits a stable JSON array of `{number, title, url, repo, item_id}`. The
`item_id` is the runtime Projects v2 item node ID for follow-up board operations; it is never
written to the repository.

Board content is untrusted input (spec `40-arbitraitor-integration.md` §6.1): titles and
bodies are carried as inert payload data, never executed and never interpolated into commands.

When live checks point at GHES or a test double, `--github-graphql-endpoint <url>` (or
`ORCHESTRAITOR_GITHUB_GRAPHQL_ENDPOINT`) overrides the API endpoint.
