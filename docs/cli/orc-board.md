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
orc board guarded-move <item-id> --status "<Status option name>" --session <session-label>
                       [--json]
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
  looped or auto-broken. Cycle detection uses back-edge detection: a diamond-shaped
  dependency DAG (two branches re-converging on one blocker) is acyclic and is NOT reported
  as a cycle; only a genuine path back into the current walk is. Every invocation is
  recorded in the event store as a `ToolRequest`
  event carrying the tool name, the filters as data, and a result summary, with the §9.25.1
  delegation chain (`user:cli -> session:orc-board-query` for the CLI surface). The chain is
  CLIENT-ASSERTED: the tool records it under `chain_source: client-asserted` with a
  `claimed:` prefix per label — it never fabricates verified identity. Filter
  values are untrusted input (spec `40-arbitraitor-integration.md` §6.1): they are matched
  as opaque data — an instruction-shaped value simply fails to match; it is never parsed,
  executed, or interpreted, and its text is truncated in the audit record.
  **Provider note:** in this slice `orc board query` reads a deterministic in-memory fixture
  board (typed fields, a blocked chain, a cycle branch) so the tool surface is testable
  offline; the sqlite provider is a #318 follow-up and the live GitHub provider wiring lands
  separately. The MCP gateway exposes the same tool as `board.query` when a
  `GatewayContext.board` provider is configured. When none is configured, the gateway's
  tool router DISABLES the route (rmcp `disable_route`): `board.query` is hidden from
  `tools/list` and invoking it is rejected with the router's typed "tool not found" error —
  the additive disable path (verified against rmcp 2.2.0 and asserted in a probe test).
  **Event persistence note:** in this slice the invocation event is written and
  hash-chain-validated but the store is per-invocation (CLI: per process) — durable
  persistence lands with the daemon event-store wiring (§9.17).
- `orc board guarded-move` is the `board.move` coordinator decision tool (spec
  `10-orchestrator.md` §9.39, issue #333): a GUARDED board transition — never a raw
  provider write. The tool validates the requested status-class transition against
  the workflow-policy transition matrix (scheduling forward `Ready -> In Progress`,
  pause/resume between `In Progress`/`Blocked`, completion `In Progress -> Done`,
  retirement; everything else — including anything touching `Triage`, the human/PM
  gate (§9.38), reopening `Done`, and unclassified columns — refuses), enforces the
  §9.40 unresolved-blocker rule (an item with any unresolved native `blockedBy` edge
  cannot enter In Progress; a `Done` blocker no longer blocks), and lease-checks
  against the session lease registry (§9.24.2 — runtime state, local-only per
  §9.43): another session's live lease refuses with `lease-conflict` naming the
  holder; the session's own expired lease refuses with `lease-expired`. An applied
  transition writes through the provider and is verified by read-back —
  reconcile-visible, board-wins on the next tick (§9.43). Every invocation —
  applied, refused, OR indeterminate — records to the event store as a `ToolRequest`
  event with the §9.25.1 delegation chain (`chain_source: client-asserted`,
  `claimed:`-prefixed labels), same mechanism as `board.query`. Refusals are typed outcomes with static
  log-safe reason classes (`policy-invalid` with the blocker ids, `lease-conflict`,
  `lease-expired`, `missing-session`, `unknown-status`, `unknown-item`,
  `provider-rejected`, `out-of-scope`); the board is unchanged after any refusal.
  Refusals are completed decisions, not process failures: the CLI renders them and
  exits 0 in both text and `--json` modes (automation reads the typed `REFUSED`
  line / the `outcome` field, not the exit status). A write that LANDS but cannot
  be verified (read-back failure or concurrent drift) is reported as a typed
  `INDETERMINATE` outcome — the board state is unknown, never reported as an
  unchanged refusal; re-read the board before retrying. A landed write whose lease
  bookkeeping fails afterwards reports the applied transition plus a typed
  lease-bookkeeping warning. Scope is status-class transitions only — request
  payload fields for field or edge
  writes are refused `out-of-scope`, never silently narrowed (issue #333
  non-goals). Item ids, status names, and session labels are untrusted input (§6.1):
  matched as opaque data — a hostile status name matches nothing and refuses
  `unknown-status`; its text never executes. **Lease semantics (§9.24.2, §9.40):**
  the check-and-claim is ONE atomic registry operation (concurrent sessions can
  never both move an unleased item); `In Progress` AND the held states
  (`Blocked`, `Approval Required`, `Input Required`) are lease-protected — a pause
  into a held state KEEPS the session's lease, so another session cannot claim the
  paused item; the lease releases on completion, retirement, or returning to Ready.
  The caller's §9.25.1 delegation-chain labels ride the request (`delegation_chain`
  on the gateway tool; `claimed:`-prefixed, truncated, bounded in the audit record —
  client-asserted data, never authorization). **Provider note:** in this slice the
  tool runs against the deterministic in-memory fixture board (the sqlite provider
  is the #318 follow-up and the live GitHub provider wiring lands separately). The
  MCP gateway exposes the same tool as `board.move` when a board provider AND lease
  registry are configured; when either is missing, the gateway's tool router
  disables BOTH board tool routes (hidden from `tools/list`, calls rejected).

`--json` on `ready` emits a stable JSON array of `{number, title, url, repo, item_id}`. The
`item_id` is the runtime Projects v2 item node ID for follow-up board operations; it is never
written to the repository.

Board content is untrusted input (spec `40-arbitraitor-integration.md` §6.1): titles and
bodies are carried as inert payload data, never executed and never interpolated into commands.

When live checks point at GHES or a test double, `--github-graphql-endpoint <url>` (or
`ORCHESTRAITOR_GITHUB_GRAPHQL_ENDPOINT`) overrides the API endpoint.
