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
orc board query [--blocked-by <item-id>] [--item-type <type>] [--status <name>]
                [--field <name>=<option>]... [--json]
```

- `orc board ready` lists items on the shared board that are leaf Task/Bug (native issue type
  wins over labels; the lowercase `task`/`bug` labels are the fallback while org-level types
  are unset), `Target=MVP`, `Status=Ready`, and have no unresolved `blockedBy` edges (open
  blockers and truncated blocker windows both exclude). Only OPEN issues qualify: a closed
  issue is excluded with a warning even when its board fields say Ready/MVP. Only the
  repositories listed under `[project].repos` are in scope, and results sort by issue number.
  Malformed or undecidable items are skipped with a warning on stderr, never a crash.
- `orc board move` finds the board item for an issue number in the configured repositories
  (searching every configured repo; ambiguous same-numbered issues across them fail with a
  typed error rather than picking one), writes the Status single-select field via
  `updateProjectV2ItemFieldValue`, and verifies the write by reading the field back before
  reporting success.
- `orc board query` is the `board.query` coordinator decision tool (spec
  `10-orchestrator.md` §9.39, issue #332): a READ-ONLY typed query over a `BoardProvider`
  (§9.43) with two modes. Filter mode (`--item-type`, `--status`, `--field name=option`,
  conjunctive) returns typed items with id, type, title, status, typed field values, and
  direct `blockedBy` edges. Blocked-graph mode (`--blocked-by <item-id>`) walks the board's
  native `blockedBy` edges transitively (§9.40 — the board's edges ARE the dependency graph;
  no mirrored second DAG): the walk is cycle-safe (visited set, depth cap) and a cycle is
  surfaced as a typed `CYCLE` indication — board-data corruption per §9.40, never silently
  looped or auto-broken. Every invocation is recorded in the event store as a `ToolRequest`
  event carrying the tool name, the filters as data, and a result summary, with the §9.25.1
  delegation chain (`user:cli -> session:orc-board-query` for the CLI surface). Filter
  values are untrusted input (spec `40-arbitraitor-integration.md` §6.1): they are matched
  as opaque data — an instruction-shaped value simply fails to match; it is never parsed,
  executed, or interpreted.
  **Provider note:** in this slice `orc board query` reads a deterministic in-memory fixture
  board (typed fields, a blocked chain, a cycle branch) so the tool surface is testable
  offline; the sqlite provider is a #318 follow-up and the live GitHub provider wiring lands
  separately. The MCP gateway exposes the same tool as `board.query` when a
  [`GatewayContext.board`](../../crates/orchestraitor-mcp/src/gateway.rs) provider is
  configured; the tool is absent (disabled) when none is.

`--json` on `ready` emits a stable JSON array of `{number, title, url, repo, item_id}`. The
`item_id` is the runtime Projects v2 item node ID for follow-up board operations; it is never
written to the repository.

Board content is untrusted input (spec `40-arbitraitor-integration.md` §6.1): titles and
bodies are carried as inert payload data, never executed and never interpolated into commands.

When live checks point at GHES or a test double, `--github-graphql-endpoint <url>` (or
`ORCHESTRAITOR_GITHUB_GRAPHQL_ENDPOINT`) overrides the API endpoint.
