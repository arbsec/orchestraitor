//! In-process MCP gateway logic for MVP.

use orchestraitor_arbitraitor_client::ArbitraitorClient;
use orchestraitor_board_contract::BoardProvider;
use orchestraitor_events::AuditStore as _;
use orchestraitor_model::OperationId;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ErrorData};
use rmcp::{ServerHandler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;
// tokio (MIT OR Apache-2.0): `spawn_blocking` runs the BLOCKING §9.48
// aggregation engine off the async runtime (spec §9.39; issue #614).
use tokio::task::spawn_blocking;

use crate::board_move::{
    BoardMoveDelegationChain, BoardMoveError, BoardMoveRequest, BoardMoveResult, board_move,
    build_invocation_event as build_move_invocation_event, execute_move,
};
use crate::board_query::{
    BoardQueryMode, BoardQueryResultKind, DelegationChain, board_query, build_invocation_event,
    execute_query, invocation_summary,
};
use crate::config::ResolvedMcpServers;
use crate::decision_record::{DecisionRecordError, DecisionRecordInput, record_decision};
use crate::error::{McpGatewayError, McpGatewayResult};
use crate::fs::FileSystemTools;
use crate::fs_types::ApplyPatchRequest;
use crate::project::{ProjectId, ProjectScope, require_server_project};
use crate::workflow::{WorkflowKind, WorkflowRequest, WorkflowTools};

/// Gateway context resolved for one project-scoped connection.
#[derive(Clone)]
pub struct GatewayContext {
    /// Project scope.
    pub scope: ProjectScope,
    /// Project-specific server set.
    pub servers: ResolvedMcpServers,
    /// Arbitraitor adapter. Security decisions remain delegated to this adapter.
    pub arbitraitor: ArbitraitorClient,
    /// Board provider the `board.query` decision tool reads through
    /// (spec `10-orchestrator.md` §9.39, §9.43; issue #332). `None` disables
    /// the tool: the gateway's tool router drops `board.query` from
    /// `tools/list` (rmcp `disable_route`), so the tool is invisible AND
    /// uncallsable — verified against rmcp 3.5.1, where `list_all`, `get`,
    /// and `call` all honor the disabled set.
    pub board: Option<std::sync::Arc<dyn BoardProvider>>,
    /// Shared audit store for `board.query` AND `pr.status` invocation
    /// events (§9.25.1). `None` records per-invocation-volatile (validated,
    /// then dropped with the invocation's store); persistence lands with the
    /// daemon event-store wiring (§9.17).
    pub board_audit_store:
        Option<std::sync::Arc<std::sync::Mutex<orchestraitor_events::InMemoryAuditStore>>>,
    /// Session-scoped decision-record store for `decision.record` (§9.35,
    /// issue #334). REQUIRED for the tool to be usable: shared across the
    /// connection's invocations so row ids strictly increase within the
    /// session — the append-only guarantee is observable, not just claimed.
    /// `None` DISABLES the tool (hidden from `tools/list`, calls rejected —
    /// the same `disable_route` posture as `board.query`): the gateway
    /// never opens a per-invocation store and reports success for an
    /// append it would drop.
    pub decision_store:
        Option<std::sync::Arc<std::sync::Mutex<orchestraitor_campaign::CampaignDecisionStore>>>,
    /// Shared lease registry for the `board.move` decision tool
    /// (§9.24.2 leases; §9.43 — runtime state is local-only, never synced
    /// to the board). `None` disables the tool the same way a missing
    /// board provider does: the route is dropped from `tools/list` and
    /// calls are rejected.
    pub board_lease_registry:
        Option<std::sync::Arc<std::sync::Mutex<crate::board_move::InMemoryLeaseRegistry>>>,
    /// The §9.48 GraphQL transport the `pr.status` decision tool reads
    /// through (spec `10-orchestrator.md` §9.39, §9.48; issue #614 slice 2).
    /// REQUIRED together with `pr_status_repo_root` AND
    /// `pr_status_allowed_repo`: any of the three missing disables the tool
    /// (the same `disable_route` posture as `board.query`: hidden from
    /// `tools/list`, calls rejected).
    pub pr_status: Option<std::sync::Arc<dyn crate::pr_status::PrStatusTransport>>,
    /// The CLEAN-record root the `pr.status` engine reads
    /// `.orchestraitor/reviews/` under — the caller's workspace config
    /// records it (the CLI passes its checkout toplevel). REQUIRED together
    /// with `pr_status` AND `pr_status_allowed_repo` for the tool to be
    /// usable.
    pub pr_status_repo_root: Option<std::path::PathBuf>,
    /// The `owner/name` repository the `pr.status` tool may read (the
    /// workspace config's project scope). REQUIRED together with
    /// `pr_status` AND `pr_status_repo_root`: tool arguments are untrusted
    /// input (§6.1), so a request naming any OTHER repository is refused
    /// with the typed `pr_status_out_of_scope` payload before the engine
    /// runs — the tool's read authority is bounded to the configured
    /// project scope, never the request's claim.
    pub pr_status_allowed_repo: Option<String>,
}

/// rmcp server exposing Orchestraitor built-in tools for one project scope.
#[derive(Clone)]
pub struct McpGateway {
    context: GatewayContext,
    fs: FileSystemTools,
    workflow: WorkflowTools,
    /// Per-instance router: the `#[tool_router]`-generated static set,
    /// with `board.query` disabled when no board provider is configured
    /// and `decision.record` disabled when no session-scoped decision
    /// store is configured (rmcp `disable_route` hides it from
    /// `list_all`/`get` and rejects `call` — the verified per-connection
    /// disable path, issue #458 F2).
    tool_router: rmcp::handler::server::router::tool::ToolRouter<Self>,
}

impl McpGateway {
    /// The `board.query` tool name, used by the router-disable path.
    const BOARD_QUERY_NAME: &'static str = "board.query";
    /// The `board.move` tool name, used by the router-disable path.
    const BOARD_MOVE_NAME: &'static str = "board.move";

    /// The `decision.record` tool name, used by the router-disable path.
    const DECISION_RECORD_NAME: &'static str = "decision.record";
    /// The `pr.status` tool name, used by the router-disable path.
    const PR_STATUS_NAME: &'static str = "pr.status";

    /// Creates a gateway for a resolved project scope.
    #[must_use]
    pub fn new(context: GatewayContext) -> Self {
        let fs = FileSystemTools::new(context.scope.clone());
        let mut tool_router = Self::static_tool_router();
        if context.board.is_none() || context.board_lease_registry.is_none() {
            // Both board decision tools need the provider; `board.move`
            // additionally needs the lease registry. Either missing
            // disables BOTH routes: the guard surface never runs against
            // a partial configuration (fail closed).
            tool_router.disable_route(Self::BOARD_QUERY_NAME);
            tool_router.disable_route(Self::BOARD_MOVE_NAME);
        }
        if context.decision_store.is_none() {
            tool_router.disable_route(Self::DECISION_RECORD_NAME);
        }
        if context.pr_status.is_none()
            || context.pr_status_repo_root.is_none()
            || context.pr_status_allowed_repo.is_none()
        {
            // `pr.status` needs BOTH the transport and the CLEAN-record
            // root: either missing disables the route — the tool never runs
            // against a partial configuration (fail closed).
            tool_router.disable_route(Self::PR_STATUS_NAME);
        }
        Self {
            context,
            fs,
            workflow: WorkflowTools::new(),
            tool_router,
        }
    }

    /// Ensures a server belongs to the current project before routing a call.
    ///
    /// # Errors
    /// Returns a cross-project isolation error when ids do not match.
    pub fn require_project_server(
        &self,
        server_project: &ProjectId,
        server_id: &str,
    ) -> McpGatewayResult<()> {
        require_server_project(&self.context.scope, server_project, server_id)
    }
}

#[tool_router(router = static_tool_router, vis = "pub(crate)")]
impl McpGateway {
    /// Read a UTF-8 project file and return content plus digest.
    ///
    /// Routes to the project-scoped filesystem tools; errors surface as
    /// structured payloads, never panics.
    /// Read a UTF-8 project file and return content plus digest.
    ///
    /// Routes to the project-scoped filesystem tools; errors surface as
    /// structured payloads, never panics.
    #[tool(
        name = "fs.read",
        description = "Read a project file and return content plus digest"
    )]
    fn fs_read(
        &self,
        Parameters(input): Parameters<PathInput>,
    ) -> Result<CallToolResult, ErrorData> {
        self.structured(self.fs.read(&input.path))
    }

    /// Return metadata for a project path.
    #[tool(name = "fs.stat", description = "Return metadata for a project path")]
    fn fs_stat(
        &self,
        Parameters(input): Parameters<PathInput>,
    ) -> Result<CallToolResult, ErrorData> {
        self.structured(self.fs.stat(&input.path))
    }

    /// List direct children of a project directory.
    #[tool(
        name = "fs.list",
        description = "List direct children of a project directory"
    )]
    /// List direct children of a project directory (structured result).
    fn fs_list(
        &self,
        Parameters(input): Parameters<PathInput>,
    ) -> Result<CallToolResult, ErrorData> {
        self.structured(self.fs.list(&input.path))
    }

    /// Search UTF-8 project files for a literal string.
    #[tool(
        name = "fs.search",
        description = "Search UTF-8 project files for a literal string"
    )]
    /// Literal substring search over UTF-8 project files; results are
    /// project-relative paths (never escaped outside the scope).
    fn fs_search(
        &self,
        Parameters(input): Parameters<SearchInput>,
    ) -> Result<CallToolResult, ErrorData> {
        self.structured(self.fs.search(&input.path, &input.query))
    }

    /// Apply a unified patch using optimistic concurrency.
    #[tool(
        name = "fs.apply_patch",
        description = "Apply a unified patch when expected_digest matches the current file digest"
    )]
    /// Apply a unified diff patch under optimistic concurrency: the call
    /// fails closed when `expected_digest` no longer matches the file.
    fn fs_apply_patch(
        &self,
        Parameters(input): Parameters<ApplyPatchRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        self.structured(self.fs.apply_patch(&input))
    }

    /// Create a new project file.
    #[tool(name = "fs.create", description = "Create a new project file")]
    fn fs_create(
        &self,
        Parameters(input): Parameters<CreateInput>,
    ) -> Result<CallToolResult, ErrorData> {
        self.structured(self.fs.create(&input.path, &input.content))
    }

    #[tool(name = "fs.rename", description = "Rename a project file or directory")]
    /// Rename (move) a project file or directory within the project scope.
    fn fs_rename(
        &self,
        Parameters(input): Parameters<RenameInput>,
    ) -> Result<CallToolResult, ErrorData> {
        self.structured(self.fs.rename(&input.from, &input.to))
    }

    #[tool(name = "fs.remove", description = "Remove a project file or directory")]
    /// Remove a project file or directory; the project root itself is
    /// never removable.
    fn fs_remove(
        &self,
        Parameters(input): Parameters<PathInput>,
    ) -> Result<CallToolResult, ErrorData> {
        self.structured(self.fs.remove(&input.path))
    }

    /// Run configured formatter after Arbitraitor inspection approves execution.
    #[tool(
        name = "format.run",
        description = "Run the configured formatter via policy-mediated execution"
    )]
    /// `format.run`: invoke the configured formatter through Arbitraitor
    /// policy mediation (inspection before execution).
    fn format_run(&self) -> Result<CallToolResult, ErrorData> {
        self.structured(
            self.workflow
                .run(WorkflowKind::Format, &WorkflowRequest { name: None }),
        )
    }

    /// Run configured linter after Arbitraitor inspection approves execution.
    #[tool(
        name = "lint.run",
        description = "Run the configured linter via policy-mediated execution"
    )]
    /// `lint.run`: invoke the configured linter through Arbitraitor
    /// policy mediation (inspection before execution).
    fn lint_run(&self) -> Result<CallToolResult, ErrorData> {
        self.structured(
            self.workflow
                .run(WorkflowKind::Lint, &WorkflowRequest { name: None }),
        )
    }

    /// Run configured check after Arbitraitor inspection approves execution.
    #[tool(
        name = "check.run",
        description = "Run configured checks via policy-mediated execution"
    )]
    /// `check.run`: invoke configured checks through Arbitraitor policy
    /// mediation (inspection before execution).
    fn check_run(&self) -> Result<CallToolResult, ErrorData> {
        self.structured(
            self.workflow
                .run(WorkflowKind::Check, &WorkflowRequest { name: None }),
        )
    }

    /// Run configured tests after Arbitraitor inspection approves execution.
    #[tool(
        name = "test.run",
        description = "Run configured tests via policy-mediated execution"
    )]
    /// `test.run`: invoke the configured test task through Arbitraitor
    /// policy mediation (inspection before execution).
    fn test_run(&self) -> Result<CallToolResult, ErrorData> {
        self.structured(
            self.workflow
                .run(WorkflowKind::Test, &WorkflowRequest { name: None }),
        )
    }

    /// Run a named task after Arbitraitor inspection approves execution.
    #[tool(
        name = "task.run",
        description = "Run a named project task via policy-mediated execution"
    )]
    /// `task.run`: invoke one named project task through Arbitraitor
    /// policy mediation (inspection before execution).
    fn task_run(
        &self,
        Parameters(input): Parameters<WorkflowRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        self.structured(self.workflow.run(WorkflowKind::Task, &input))
    }

    /// `decision.record` (spec `10-orchestrator.md` §9.39, §9.35; issue
    /// #334): persist ONE append-only, replayable decision record into the
    /// campaign crate's §9.35 store — the SAME store `orc campaign run
    /// --once` writes. Requires the session-scoped decision store on the
    /// context: without one the tool is disabled (hidden from
    /// `tools/list`, calls rejected) and the shared runner refuses with a
    /// typed `decision_record_unconfigured` error — the gateway never
    /// opens a per-invocation store and reports success for an append it
    /// would drop. Malformed records are refused with typed reasons
    /// (nothing is appended); secret-shaped material is refused
    /// (fail-closed). Every successful append is recorded as a `ToolRequest`
    /// event with the §9.25.1 delegation chain. There is no update or
    /// delete path: re-recording appends a NEW row.
    #[tool(
        name = "decision.record",
        description = "Persist one append-only, replayable campaign decision record (kind, selected task, role, model+provider, worker arguments, rationale, alternatives) per spec §9.35"
    )]
    fn decision_record_tool(
        &self,
        Parameters(input): Parameters<DecisionRecordRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let chain = DelegationChain {
            correlation_id: OperationId::new(),
            parent_op_id: None,
            principals: input.delegation_chain.clone(),
        };
        let result = run_decision_record_shared(
            &input.record,
            &chain,
            self.context.decision_store.as_ref(),
            self.context.board_audit_store.as_ref(),
        );
        self.structured(result)
    }

    /// Query board state through the configured [`BoardProvider`]: typed
    /// conjunctive search or the transitive blocked graph (spec
    /// `10-orchestrator.md` §9.39, §9.40, §9.43; issue #332). Read-only;
    /// filter values are opaque data. When no board provider is configured
    /// in the context, the route is disabled entirely (hidden from
    /// `tools/list`, calls rejected) — this arm is defense in depth.
    #[tool(
        name = "board.query",
        description = "Read-only board query: typed search (item type, status, field values) or the transitive blockedBy graph for one item"
    )]
    /// `board.query`: the typed read-only board query tool (see
    /// [`run_board_query_shared`] for recording semantics).
    async fn board_query(
        &self,
        Parameters(input): Parameters<BoardQueryRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        match &self.context.board {
            Some(provider) => {
                let provider = provider.clone();
                let mode = input.mode;
                let chain = input.delegation_chain;
                // rmcp's tool macro supports async fns; the tool body stays
                // async rather than bridging runtimes (block_in_place panics
                // on a current_thread runtime).
                //
                // A shared audit store (§9.25.1, F3) is drained under its
                // lock BEFORE the await: MutexGuard is not Send, so the
                // store's records are cloned into the invocation and merged
                // back after — a short synchronous section, no guard held
                // across an await point.
                let shared = self.context.board_audit_store.clone();
                let result = run_board_query_shared(provider.as_ref(), &mode, chain, shared).await;
                self.structured(result)
            }
            None => Ok(CallToolResult::structured_error(serde_json::json!({
                "error": "board.query is not configured for this project scope",
                "code": "board_query_unconfigured"
            }))),
        }
    }

    /// Apply a guarded board transition through the configured
    /// [`BoardProvider`](orchestraitor_board_contract::BoardProvider):
    /// workflow-policy validated, lease-checked, reconcile-visible (spec
    /// `10-orchestrator.md` §9.39, §9.40, §9.43; issue #333) — never a raw
    /// provider write. Refusals are typed structured data with the board
    /// unchanged. When no board provider AND lease registry are configured
    /// in the context, the route is disabled entirely — this arm is
    /// defense in depth.
    #[tool(
        name = "board.move",
        description = "Guarded board transition: move one item to a status class after workflow-policy validation and lease check; refusals are typed, board stays unchanged"
    )]
    async fn board_move(
        &self,
        Parameters(input): Parameters<BoardMoveRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let Some(provider) = self.context.board.clone() else {
            return Ok(CallToolResult::structured_error(serde_json::json!({
                "error": "board.move is not configured for this project scope",
                "code": "board_move_unconfigured"
            })));
        };
        let Some(registry) = self.context.board_lease_registry.clone() else {
            return Ok(CallToolResult::structured_error(serde_json::json!({
                "error": "board.move is not configured for this project scope",
                "code": "board_move_unconfigured"
            })));
        };
        let shared = self.context.board_audit_store.clone();
        let result = run_board_move_shared(provider.as_ref(), registry, input, shared).await;
        self.structured(result)
    }

    /// `pr.status` (spec `10-orchestrator.md` §9.39, §9.48; issue #614
    /// slice 2): the coordinator decision tool over the SHARED §9.48
    /// aggregation engine — the same typed verdict, the same reason codes,
    /// the same fail-closed unread sub-check semantics as `orc pr status`.
    /// Read-only: the tool never labels, comments, reviews, or merges.
    /// `owner`/`repo`/`number` are opaque untrusted data (§6.1) passed
    /// through to the engine, which fail-closes on unreadable PRs. The
    /// engine is BLOCKING, so the call runs inside
    /// `tokio::task::spawn_blocking`. Every invocation records a
    /// `ToolRequest` event into the shared audit store with the
    /// §9.25.1 delegation chain (mirroring `board.query`). When no
    /// transport + CLEAN-record root are configured in the context, the
    /// route is disabled entirely — this arm is defense in depth. A
    /// concurrent-edit anchor movement is the typed
    /// `pr_status_concurrent_edit` payload; an unread sub-check carries the
    /// degraded partial verdict on the `partial_verdict` field so a caller
    /// can read it (§9.48 invariant).
    #[tool(
        name = "pr.status",
        description = "Read-only PR mergeability verdict for one pull request: classified checks, review threads, checklist reconciliation, CLEAN-review-record state, and the aggregate convergence (mergeable + reason codes)"
    )]
    async fn pr_status_tool(
        &self,
        Parameters(input): Parameters<PrStatusRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let (Some(transport), Some(repo_root), Some(allowed_repo)) = (
            self.context.pr_status.clone(),
            self.context.pr_status_repo_root.clone(),
            self.context.pr_status_allowed_repo.clone(),
        ) else {
            return Ok(CallToolResult::structured_error(serde_json::json!({
                "error": "pr.status is not configured for this project scope",
                "code": "pr_status_unconfigured"
            })));
        };
        // Scope binding (§6.1): the request's owner/repo are UNTRUSTED —
        // refuse any repository outside the configured project scope before
        // the engine runs. The comparison is exact (case-sensitive) against
        // the configured slug; the mismatch payload names the configured
        // scope, never echoes the refused request values back as authority.
        if format!("{}/{}", input.owner, input.repo) != allowed_repo {
            // Refused scope-boundary probes are the events an auditor needs
            // most: record the refusal with the out_of_scope outcome class
            // before returning the typed payload. The refused request values
            // ride the payload as data (truncated), never as authority.
            let shared = self.context.board_audit_store.clone();
            let correlation_id = OperationId::new();
            let chain = DelegationChain {
                correlation_id: correlation_id.clone(),
                parent_op_id: None,
                principals: input.delegation_chain.clone(),
            };
            // Fail closed: an audit failure must be VISIBLE — the refusal
            // payload carries an `audit_recorded: false` marker instead of
            // silently dropping the trail. The shared store is locked
            // synchronously (no guard crosses an await).
            let mut audit_recorded = false;
            if let Some(shared) = shared.as_ref()
                && let Ok(mut store) = shared.lock()
                && let Ok(event) = build_pr_status_refusal_event(
                    &input,
                    &chain,
                    &allowed_repo,
                    store.records().len(),
                    store.records().last().map(|record| record.hash.clone()),
                )
            {
                audit_recorded = store.append(event).is_ok();
            }
            let mut refusal = serde_json::json!({
                "error": format!(
                    "pr.status is bound to {allowed_repo}; the requested repository is \
                     outside this project scope"
                ),
                "code": "pr_status_out_of_scope",
                "allowed": allowed_repo,
            });
            refusal["audit_recorded"] = serde_json::Value::Bool(audit_recorded);
            return Ok(CallToolResult::structured_error(refusal));
        }
        let shared = self.context.board_audit_store.clone();
        match run_pr_status_shared(transport, repo_root, input, shared).await {
            Ok(verdict) => {
                self.structured(Ok::<crate::pr_status::PrStatus, McpGatewayError>(verdict))
            }
            Err(tool_error) => Ok(CallToolResult::structured_error(tool_error.payload)),
        }
    }
}

// The lint fires on rmcp 3.x's macro-generated trait methods, not on code we
// control here — silencing at the impl boundary is the tightest available
// scope. `unknown_lints` keeps 1.96 (which predates the lint) compiling.
#[allow(unknown_lints)]
#[allow(clippy::unused_async_trait_impl)]
#[tool_handler(router = self.tool_router, name = "orchestraitor-mcp", version = "0.0.0")]
impl ServerHandler for McpGateway {}

impl McpGateway {
    /// Wraps a typed result into an rmcp `CallToolResult`: `Ok` serializes
    /// to structured content, `Err` maps through [`error_payload`] so tool
    /// failures are structured data, never exceptions.
    fn structured<T>(&self, result: McpGatewayResult<T>) -> Result<CallToolResult, ErrorData>
    where
        T: Serialize,
    {
        let _ = self;
        match result {
            Ok(value) => serde_json::to_value(value)
                .map(CallToolResult::structured)
                .map_err(|error| ErrorData::internal_error(error.to_string(), None)),
            Err(error) => Ok(CallToolResult::structured_error(error_payload(&error))),
        }
    }
}

#[derive(Debug, Clone, serde::Deserialize, JsonSchema)]
/// Request shape for single-path tools (`fs.read`, `fs.stat`, `fs.list`,
/// `fs.remove`): one project-relative path.
struct PathInput {
    path: String,
}

#[derive(Debug, Clone, serde::Deserialize, JsonSchema)]
/// Request shape for `fs.search`: a project-relative path plus a literal
/// query string.
struct SearchInput {
    path: String,
    query: String,
}

#[derive(Debug, Clone, serde::Deserialize, JsonSchema)]
/// Request shape for `fs.create`: a project-relative path and the file
/// content to write.
struct CreateInput {
    path: String,
    content: String,
}

#[derive(Debug, Clone, serde::Deserialize, JsonSchema)]
/// Request shape for `fs.rename`: source and destination project-relative
/// paths.
struct RenameInput {
    from: String,
    to: String,
}

/// `board.query` request shape: the mode plus the §9.25.1 delegation-chain
/// labels supplied by the invoking session (recorded verbatim as data).
#[derive(Debug, Clone, serde::Deserialize, JsonSchema)]
struct BoardQueryRequest {
    /// Query mode: `search` with a typed filter, or `blocked_by` with one
    /// item id.
    mode: BoardQueryMode,
    /// Delegation-chain principal labels, root first (e.g.
    /// `["user:alice", "session:sess_7e3f"]`). Static labels only.
    #[serde(default)]
    delegation_chain: Vec<String>,
}

/// `decision.record` request shape: the §9.35 record fields (flattened —
/// the MCP schema is DERIVED from the validated record shape, so the two
/// cannot drift) plus the §9.25.1 delegation-chain labels supplied by the
/// invoking session (recorded as data on the audit event, never treated as
/// authority).
#[derive(Debug, Clone, serde::Deserialize, JsonSchema)]
struct DecisionRecordRequest {
    /// The §9.35 record fields, deserialized in place.
    #[serde(flatten)]
    record: DecisionRecordInput,
    /// Delegation-chain principal labels, root first. Client-asserted data
    /// only — the tool never mints or verifies identity.
    #[serde(default)]
    delegation_chain: Vec<String>,
}

/// Drives one `decision.record` append (issue #334).
///
/// The store is the campaign crate's append-only §9.35 `SQLite` store; the
/// session-scoped store on the context is REQUIRED. With it, every
/// invocation on the connection appends into THAT store (row ids strictly
/// increase across the session — the append-only guarantee is observable)
/// and its records replay for the connection's lifetime. Without one, the
/// invocation is REFUSED with [`McpGatewayError::DecisionRecordUnconfigured`]:
/// appending into a per-invocation in-memory store and reporting success
/// would fabricate persistence — the record would be dropped when the call
/// returns while the caller believes it is stored (spec §9.35
/// persist+replay). DURABLE persistence stays owned by `orc campaign run
/// --once` (which opens the same store at `<config-dir>/campaign.db`);
/// wiring the MCP tool to that same on-disk store lands with the daemon
/// decision-tool wiring (§9.17/§9.39) — the record shape and code path are
/// identical, only the store handle differs.
pub(crate) fn run_decision_record_shared(
    input: &DecisionRecordInput,
    chain: &DelegationChain,
    shared_store: Option<
        &std::sync::Arc<std::sync::Mutex<orchestraitor_campaign::CampaignDecisionStore>>,
    >,
    shared_audit: Option<
        &std::sync::Arc<std::sync::Mutex<orchestraitor_events::InMemoryAuditStore>>,
    >,
) -> Result<orchestraitor_campaign::StoredCampaignDecision, McpGatewayError> {
    // §9.25.1 recording mirrors `board.query` (issue #458 F3): with a shared
    // audit store the ToolRequest event is appended DIRECTLY into that
    // store under ONE lock section held across the whole invocation, so it
    // CONTINUES the shared hash chain (seq = len+1, prev_hash = last shared
    // hash), survives the invocation, and concurrent invocations serialize
    // without loss (N2) — no snapshot/replay window exists. With no shared
    // store the recording is PER-INVOCATION-VOLATILE — written and
    // validated into an invocation-local store, then dropped (same
    // documented posture as `board.query`). Durable persistence lands with
    // the daemon event-store wiring (§9.17). All sections here are
    // synchronous; no guard ever crosses an await point.
    // The session-scoped decision store is REQUIRED (see the function
    // contract): no store means no persistence, and the tool never reports
    // success for an append it is about to drop. Checked FIRST so an
    // unconfigured store returns without taking the shared audit lock.
    let Some(shared_store) = shared_store else {
        return Err(McpGatewayError::DecisionRecordUnconfigured);
    };
    let mut local_audit = orchestraitor_events::InMemoryAuditStore::default();
    let mut shared_guard =
        match shared_audit {
            Some(shared) => Some(shared.lock().map_err(|_| {
                McpGatewayError::DecisionRecord(String::from("audit store poisoned"))
            })?),
            None => None,
        };
    let audit_target: &mut (dyn orchestraitor_events::AuditStore + Send) =
        match shared_guard.as_mut() {
            Some(guard) => &mut **guard,
            None => &mut local_audit,
        };

    let mut store = shared_store
        .lock()
        .map_err(|_| McpGatewayError::DecisionRecord(String::from("decision store poisoned")))?;
    record_decision(input, chain, &mut store, audit_target)
        .map_err(|error| decision_record_error(&error))
}

/// Maps a `decision.record` failure onto the typed gateway error. The
/// message is a static, log-safe label — never record content (§9.23.4).
fn decision_record_error(error: &DecisionRecordError) -> McpGatewayError {
    McpGatewayError::DecisionRecord(error.to_string())
}

/// Drives the typed board query against a provider, recording into the
/// context's audit store when one is shared, otherwise a fresh in-memory
/// store (issue #458 F3).
///
/// With no shared store the recording is PER-INVOCATION-VOLATILE: the event
/// is written and validated, then dropped with the store. Persistence lands
/// with the daemon event-store wiring (§9.17), not in this slice.
///
/// Shared-store merge (issues #458-gen2 N1/N2): the invocation event is
/// built to CONTINUE that chain (sequence = len+1, `prev_hash` = last shared
/// hash) and appended under one lock section. No drain-replay round trip and
/// no blind overwrite: concurrent invocations serialize on the mutex, so
/// seed events and one `ToolRequest` per invocation all survive. The store's
/// `import` is not used here — its whole-chain validation requires seq=1,
/// which a shared store's history violates by design. The lock is held only
/// across synchronous section(s); `MutexGuard` is not Send and never crosses
/// an await. A poisoned lock fails the invocation closed.
pub(crate) async fn run_board_query_shared(
    provider: &dyn BoardProvider,
    mode: &BoardQueryMode,
    principals: Vec<String>,
    shared_store: Option<
        std::sync::Arc<std::sync::Mutex<orchestraitor_events::InMemoryAuditStore>>,
    >,
) -> Result<BoardQueryResultKind, McpGatewayError> {
    let correlation_id = OperationId::new();
    let chain = DelegationChain {
        correlation_id,
        parent_op_id: None,
        principals,
    };
    if let Some(shared) = shared_store {
        // Run the read FIRST (no locks held); then snapshot + append under
        // ONE lock section so the event continues the chain exactly as it
        // stands and concurrent invocations serialize without loss (N2).
        // The shared store is never overwritten: seed events and other
        // invocations' events survive.
        let result = execute_query(provider, mode)
            .await
            .map_err(|error| McpGatewayError::BoardQuery(error.to_string()))?;
        let summary = invocation_summary(&result);

        {
            let mut store = shared
                .lock()
                .map_err(|_| McpGatewayError::BoardQuery(String::from("audit store poisoned")))?;
            let seq_base = store.records().len();
            let prev_base = store.records().last().map(|record| record.hash.clone());
            let event = build_invocation_event(mode, &chain, &summary, seq_base, prev_base)
                .map_err(|_| {
                    McpGatewayError::BoardQuery(String::from(
                        "invocation event construction failed",
                    ))
                })?;
            store.append(event).map_err(|_| {
                McpGatewayError::BoardQuery(String::from("invocation event append rejected"))
            })?;
        }
        return Ok(result);
    }
    let mut store = orchestraitor_events::InMemoryAuditStore::default();
    board_query(provider, mode, &chain, &mut store)
        .await
        .map_err(|error| McpGatewayError::BoardQuery(error.to_string()))
}

/// Drives one guarded board move against a provider, recording into the
/// context's audit store when one is shared, otherwise a fresh in-memory
/// store (mirrors [`run_board_query_shared`], issue #458 F3).
///
/// The caller's §9.25.1 delegation-chain labels ride ON the request
/// (`delegation_chain`, client-asserted data — PR #475 review thread 7)
/// and are recorded verbatim with the same `claimed:` prefix and
/// truncation bounds as `board.query`.
///
/// The move runs FIRST (no locks held), then the outcome — applied,
/// refused, OR indeterminate; every terminal outcome is a decision and
/// records — appends under ONE lock section. The lock is held only across
/// the synchronous section; a poisoned lock fails the invocation closed.
/// An append failure on a shared store maps to the typed indeterminate
/// error: the board mutation (when applied) already landed and the audit
/// gap must be visible (PR #475 review thread 1).
pub(crate) async fn run_board_move_shared(
    provider: &dyn orchestraitor_board_contract::BoardProvider,
    registry: std::sync::Arc<std::sync::Mutex<crate::board_move::InMemoryLeaseRegistry>>,
    request: BoardMoveRequest,
    shared_store: Option<
        std::sync::Arc<std::sync::Mutex<orchestraitor_events::InMemoryAuditStore>>,
    >,
) -> Result<BoardMoveResult, McpGatewayError> {
    let correlation_id = OperationId::new();
    let chain = BoardMoveDelegationChain {
        correlation_id,
        parent_op_id: None,
        principals: request.delegation_chain.clone(),
    };
    if let Some(shared) = shared_store {
        let mut guarded = GuardedRegistry(std::sync::Arc::clone(&registry));
        let (outcome, summary) = execute_move(provider, &mut guarded, &request)
            .await
            .map_err(|error| McpGatewayError::BoardMove(error.to_string()))?;
        {
            let mut store = shared
                .lock()
                .map_err(|_| McpGatewayError::BoardMove(String::from("audit store poisoned")))?;
            let seq_base = store.records().len();
            let prev_base = store.records().last().map(|record| record.hash.clone());
            let event =
                build_move_invocation_event(&request, &chain, &summary, seq_base, prev_base)
                    .map_err(|_| {
                        McpGatewayError::BoardMove(String::from(
                            "invocation event construction failed",
                        ))
                    })?;
            store.append(event).map_err(|_| {
                // The append failed AFTER the move's terminal outcome: the
                // message must match the outcome class (PR #478 review).
                // Refused: the board is provably unchanged — only the
                // record is missing. Applied/indeterminate: the board
                // mutation may have landed, so the audit gap means the
                // board state must be reconciled through a fresh read
                // (PR #475 review thread 1). Either way the error return
                // keeps the §9.39 fail-closed posture: the invocation is
                // never reported as an unrecorded success.
                let message = match &outcome {
                    crate::board_move::BoardMoveOutcome::Refused { .. } => {
                        "invocation event append rejected after a refused move; \
                         the board is unchanged but the invocation was not recorded"
                    }
                    crate::board_move::BoardMoveOutcome::Applied { .. }
                    | crate::board_move::BoardMoveOutcome::Indeterminate { .. } => {
                        "invocation event append rejected: board state for this \
                         move must be reconciled through a fresh read"
                    }
                };
                McpGatewayError::BoardMove(String::from(message))
            })?;
        }
        return Ok(BoardMoveResult {
            correlation_id: chain.correlation_id.to_string(),
            outcome,
        });
    }
    let mut store = orchestraitor_events::InMemoryAuditStore::default();
    let mut guarded = GuardedRegistry(registry);
    board_move(provider, &mut guarded, &request, &chain, &mut store)
        .await
        .map_err(|error| McpGatewayError::BoardMove(error.to_string()))
}

/// `pr.status` request shape (spec §9.48): one pull request plus the
/// §9.25.1 delegation-chain labels supplied by the invoking session
/// (recorded verbatim as data). `owner`/`repo` are untrusted opaque data
/// (§6.1) — passed through to the engine, which fail-closes on unreadable
/// PRs; they are truncated before entering any audit record.
#[derive(Debug, Clone, serde::Deserialize, JsonSchema)]
pub(crate) struct PrStatusRequest {
    /// Pull-request number.
    number: u64,
    /// Repository owner (untrusted data).
    owner: String,
    /// Repository name (untrusted data).
    repo: String,
    /// Delegation-chain principal labels, root first (e.g.
    /// `["user:alice", "session:sess_7e3f"]`). Static labels only.
    #[serde(default)]
    delegation_chain: Vec<String>,
}

/// `pr.status` summary for the audit event's `result_summary`: the typed
/// verdict's convergence outcome, never the full provider payload.
#[derive(Debug, Clone, serde::Serialize)]
struct PrStatusInvocationSummary {
    /// Whether the engine reported the PR mergeable.
    mergeable: bool,
    /// Count of stable machine reason codes.
    reason_count: usize,
    /// The invocation's outcome class: `verdict` for a successful read,
    /// otherwise the error variant name (`unrecoverable`, `config`,
    /// `concurrent_edit`) — the audit trail shows what kind of answer the
    /// caller got without parsing payloads.
    outcome: String,
}

// The event-payload truncation helper is `board_query`'s (single source of
// truth for the bound + marker): reused directly, not copied.
use crate::board_query::truncate_payload_value as truncate_pr_status_value;

/// Builds the `ToolRequest` envelope for one `pr.status` invocation,
/// continuing the chain at `(seq_base, prev_base)` (mirrors
/// `board_query::build_invocation_event`; issues #458-gen2 N1/N2).
/// Builds the `ToolRequest` event for one OUT-OF-SCOPE refusal: the
/// refused request values ride as truncated data, the outcome class is
/// `out_of_scope`, and the configured scope is recorded — the audit trail
/// shows the scope-boundary probe without echoing the request as authority.
fn build_pr_status_refusal_event(
    request: &PrStatusRequest,
    chain: &DelegationChain,
    allowed_repo: &str,
    seq_base: usize,
    prev_base: Option<orchestraitor_events::HashDigest>,
) -> Result<orchestraitor_events::EventEnvelope, McpGatewayError> {
    let result_summary = serde_json::json!({
        "outcome": "out_of_scope",
        "allowed": allowed_repo,
    });
    build_pr_status_event(request, chain, result_summary, seq_base, prev_base)
}

fn build_pr_status_invocation_event(
    request: &PrStatusRequest,
    chain: &DelegationChain,
    summary: &PrStatusInvocationSummary,
    seq_base: usize,
    prev_base: Option<orchestraitor_events::HashDigest>,
) -> Result<orchestraitor_events::EventEnvelope, McpGatewayError> {
    let result_summary = serde_json::to_value(summary)
        .map_err(|_| McpGatewayError::PrStatus(String::from("summary serialization failed")))?;
    build_pr_status_event(request, chain, result_summary, seq_base, prev_base)
}

/// Builds the SHARED payload prefix and envelope tail for the two
/// `pr.status` event builders: tool name, truncated request values,
/// chain-source labelling, principal truncation/cap, and the envelope
/// construction — one source of truth for the truncation and chain bounds.
fn build_pr_status_event(
    request: &PrStatusRequest,
    chain: &DelegationChain,
    result_summary: serde_json::Value,
    seq_base: usize,
    prev_base: Option<orchestraitor_events::HashDigest>,
) -> Result<orchestraitor_events::EventEnvelope, McpGatewayError> {
    let mut payload = serde_json::Map::new();
    payload.insert("tool".to_string(), Value::String(String::from("pr.status")));
    payload.insert(
        "request".to_string(),
        serde_json::json!({
            "number": request.number,
            "owner": truncate_pr_status_value(&request.owner),
            "repo": truncate_pr_status_value(&request.repo),
        }),
    );
    payload.insert("result_summary".to_string(), result_summary);
    payload.insert(
        "chain_source".to_string(),
        Value::String(String::from("client-asserted")),
    );
    let mut principals: Vec<Value> = chain
        .principals
        .iter()
        .take(crate::board_query::MAX_CHAIN_PRINCIPALS)
        .map(|principal| truncate_pr_status_value(principal))
        .map(|truncated| Value::String(format!("claimed:{truncated}")))
        .collect();
    if chain.principals.len() > crate::board_query::MAX_CHAIN_PRINCIPALS {
        principals.push(Value::String(String::from(
            crate::board_query::CHAIN_TRUNCATION_MARKER,
        )));
    }
    payload.insert("delegation_chain".to_string(), Value::Array(principals));

    let monotonic_seq = u64::try_from(seq_base).map_or(u64::MAX, |base| base.saturating_add(1));
    orchestraitor_events::EventEnvelope::try_new(orchestraitor_events::EventEnvelopeInput {
        schema_version: orchestraitor_events::CURRENT_SCHEMA_VERSION,
        monotonic_seq,
        wall_clock_ts: rfc3339_now(),
        correlation_id: chain.correlation_id.clone(),
        parent_op_id: chain.parent_op_id.clone(),
        category: orchestraitor_events::EventCategory::ToolRequest,
        payload: Value::Object(payload),
        prev_hash: prev_base,
    })
    .map_err(|_| McpGatewayError::PrStatus(String::from("event construction failed")))
}

/// Current wall-clock time as an RFC 3339 string (mirrors
/// `board_query::rfc3339_now`; typed, never panicking).
fn rfc3339_now() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| String::from("1970-01-01T00:00:00Z"))
}

/// Drives one `pr.status` aggregation through the shared engine, recording
/// into the context's audit store when one is shared, otherwise a fresh
/// in-memory store (mirrors [`run_board_query_shared`], §9.25.1).
///
/// The engine is BLOCKING (its transport is a blocking reqwest client), so
/// the aggregation runs inside `tokio::task::spawn_blocking`. The read
/// runs FIRST (no locks held); then the outcome is appended under ONE lock
/// section so the event continues the shared chain exactly as it stands
/// and concurrent invocations serialize without loss. A poisoned lock or a
/// rejected append fails the invocation closed — never an unrecorded
/// success. Errors are typed [`PrStatusToolError`]s whose payload is the
/// structured tool content (never prose-only); every terminal outcome
/// records its event first.
pub(crate) async fn run_pr_status_shared(
    transport: std::sync::Arc<dyn crate::pr_status::PrStatusTransport>,
    repo_root: std::path::PathBuf,
    request: PrStatusRequest,
    shared_store: Option<
        std::sync::Arc<std::sync::Mutex<orchestraitor_events::InMemoryAuditStore>>,
    >,
) -> Result<crate::pr_status::PrStatus, PrStatusToolError> {
    let correlation_id = OperationId::new();
    let chain = DelegationChain {
        correlation_id,
        parent_op_id: None,
        principals: request.delegation_chain.clone(),
    };
    // The engine is Send + Sync over an Arc transport; spawn_blocking owns
    // cloned handles for the blocking section.
    let number = request.number;
    let owner = request.owner.clone();
    let repo = request.repo.clone();
    let aggregate_result = {
        let transport = transport.clone();
        let repo_root = repo_root.clone();
        spawn_blocking(move || {
            let engine = crate::pr_status::Engine::new(transport, repo_root);
            engine.aggregate(&owner, &repo, number)
        })
        .await
        .unwrap_or_else(|join_error| {
            // A worker failure (panic inside spawn_blocking, join error) is
            // an unrecoverable read. It becomes a TYPED Err VALUE (not `?`)
            // so the shared summary/record path below runs — every terminal
            // outcome records its event first, never a silent audit gap.
            Err(crate::pr_status::PrStatusError::Unrecoverable {
                message: format!("pr.status worker failed: {join_error}"),
                partial_json: String::new(),
            })
        })
    };

    let summary = match &aggregate_result {
        Ok(verdict) => PrStatusInvocationSummary {
            mergeable: verdict.convergence.mergeable,
            reason_count: verdict.convergence.reasons.len(),
            outcome: String::from("verdict"),
        },
        // A failed read still records its outcome class: the reason count
        // is unavailable, so the summary names zero reasons with
        // mergeable false (never a fabricated green light).
        Err(error) => PrStatusInvocationSummary {
            mergeable: false,
            reason_count: 0,
            outcome: String::from(error_outcome_class(error)),
        },
    };

    let record = |summary: &PrStatusInvocationSummary| -> Result<(), McpGatewayError> {
        if let Some(shared) = shared_store.as_ref() {
            let mut store = shared
                .lock()
                .map_err(|_| McpGatewayError::PrStatus(String::from("audit store poisoned")))?;
            let seq_base = store.records().len();
            let prev_base = store.records().last().map(|record| record.hash.clone());
            let event =
                build_pr_status_invocation_event(&request, &chain, summary, seq_base, prev_base)?;
            store.append(event).map_err(|_| {
                McpGatewayError::PrStatus(String::from("invocation event append rejected"))
            })?;
        } else {
            // Per-invocation-volatile recording (same documented posture as
            // `board.query` without a shared store).
            let mut store = orchestraitor_events::InMemoryAuditStore::default();
            let event = build_pr_status_invocation_event(&request, &chain, summary, 0, None)?;
            store.append(event).map_err(|_| {
                McpGatewayError::PrStatus(String::from("invocation event append rejected"))
            })?;
        }
        Ok(())
    };

    match aggregate_result {
        Ok(verdict) => {
            record(&summary).map_err(|error| PrStatusToolError {
                payload: error_payload(&error),
            })?;
            Ok(verdict)
        }
        Err(error) => {
            // The failed read's outcome class is recorded, then the typed
            // payload replaces the generic one: the caller sees the §9.48
            // typed error, the audit trail sees the invocation.
            let typed = pr_status_error_payload(&error);
            record(&summary).map_err(|record_error| PrStatusToolError {
                payload: error_payload(&record_error),
            })?;
            Err(typed)
        }
    }
}

/// A `pr.status` failure whose payload IS the structured tool content: a
/// typed JSON object (never prose-only) per the §9.48 contract.
#[derive(Debug)]
pub(crate) struct PrStatusToolError {
    /// The structured error payload for `CallToolResult::structured_error`.
    pub payload: serde_json::Value,
}

/// The audit outcome class for one [`PrStatusError`] (static variant
/// labels — never error message text, §9.23.4).
fn error_outcome_class(error: &crate::pr_status::PrStatusError) -> &'static str {
    use crate::pr_status::PrStatusError as E;
    match error {
        E::Unrecoverable { .. } => "unrecoverable",
        E::Config { .. } => "config",
        E::ConcurrentEdit { .. } => "concurrent_edit",
    }
}

/// Maps a typed [`PrStatusError`](crate::pr_status::PrStatusError) onto the
/// structured error payload the MCP client sees: always a typed JSON
/// object (never prose-only), with the degraded partial verdict riding the
/// `partial_verdict` field when the engine emitted one (§9.48 invariant).
fn pr_status_error_payload(error: &crate::pr_status::PrStatusError) -> PrStatusToolError {
    use crate::pr_status::PrStatusError as E;
    let mut payload = serde_json::Map::new();
    match &error {
        E::Unrecoverable {
            message,
            partial_json,
        } => {
            payload.insert("error".to_string(), Value::String(message.clone()));
            payload.insert(
                "code".to_string(),
                Value::String(String::from("pr_status_unrecoverable")),
            );
            if !partial_json.is_empty() {
                // The degraded typed verdict: parse back so it rides as
                // structured JSON, not an opaque string.
                match serde_json::from_str::<Value>(partial_json) {
                    Ok(verdict) => {
                        payload.insert("partial_verdict".to_string(), verdict);
                    }
                    Err(_) => {
                        payload.insert(
                            "partial_verdict".to_string(),
                            Value::String(partial_json.clone()),
                        );
                    }
                }
            }
        }
        E::Config { message } => {
            payload.insert("error".to_string(), Value::String(message.clone()));
            payload.insert(
                "code".to_string(),
                Value::String(String::from("pr_status_config")),
            );
        }
        E::ConcurrentEdit {
            anchor,
            before,
            after,
        } => {
            payload.insert("error".to_string(), Value::String(error.to_string()));
            payload.insert(
                "code".to_string(),
                Value::String(String::from("pr_status_concurrent_edit")),
            );
            payload.insert("anchor".to_string(), Value::String(String::from(*anchor)));
            payload.insert("before".to_string(), Value::String(before.clone()));
            payload.insert("after".to_string(), Value::String(after.clone()));
        }
    }
    PrStatusToolError {
        payload: payload.into(),
    }
}

/// Adapter that locks the shared registry long enough for one synchronous
/// registry call — `LeaseRegistry` methods are sync and short (no await
/// inside), so a per-call lock section never crosses an await point. The
/// shared registry serializes concurrent invocations at `try_claim`, so
/// the TOCTOU fix holds across gateway requests (PR #475 review thread 2).
struct GuardedRegistry(std::sync::Arc<std::sync::Mutex<crate::board_move::InMemoryLeaseRegistry>>);

impl crate::board_move::LeaseRegistry for GuardedRegistry {
    fn try_claim(
        &mut self,
        item: &orchestraitor_board_contract::BoardItemId,
        session: &str,
        expires_at_unix_secs: u64,
    ) -> Result<crate::board_move::ClaimOutcome, BoardMoveError> {
        let mut leases = self
            .0
            .lock()
            .map_err(|_| BoardMoveError::EventStore("lease registry poisoned"))?;
        leases.try_claim(item, session, expires_at_unix_secs)
    }

    fn held_by(
        &self,
        item: &orchestraitor_board_contract::BoardItemId,
        session: &str,
    ) -> Result<Option<crate::board_move::ItemLease>, BoardMoveError> {
        let leases = self
            .0
            .lock()
            .map_err(|_| BoardMoveError::EventStore("lease registry poisoned"))?;
        leases.held_by(item, session)
    }

    fn release(
        &mut self,
        item: &orchestraitor_board_contract::BoardItemId,
        session: &str,
    ) -> Result<(), BoardMoveError> {
        let mut leases = self
            .0
            .lock()
            .map_err(|_| BoardMoveError::EventStore("lease registry poisoned"))?;
        leases.release(item, session)
    }
}

/// Maps a typed gateway error onto the structured error payload the MCP
/// client sees: a human-readable message plus a stable machine `code`
/// (never board content, never internal details).
fn error_payload(error: &McpGatewayError) -> serde_json::Value {
    serde_json::json!({
        "error": error.to_string(),
        "code": match error {
            McpGatewayError::DigestMismatch { .. } => "digest_mismatch",
            McpGatewayError::CrossProjectToolLeak { .. } => "cross_project_tool_leak",
            McpGatewayError::ArbitraitorInspectionRequired { .. } => "arbitraitor_inspection_required",
            McpGatewayError::PathEscapesProject | McpGatewayError::InvalidProjectPath { .. } => "invalid_project_path",
            McpGatewayError::AlreadyExists { .. } => "already_exists",
            McpGatewayError::PatchRejected { .. } => "patch_rejected",
            McpGatewayError::Toml(_) => "invalid_mcp_toml",
            McpGatewayError::CanonicalJson { .. } => "fingerprint_canonicalization_failed",
            McpGatewayError::Io(_) => "io_error",
            McpGatewayError::BoardQuery(_) => "board_query_failed",
            McpGatewayError::DecisionRecord(_) => "decision_record_failed",
            McpGatewayError::DecisionRecordUnconfigured => "decision_record_unconfigured",
            McpGatewayError::BoardMove(_) => "board_move_failed",
            McpGatewayError::PrStatus(_) => "pr_status_failed",
        }
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    /// A fixed delegation chain for the decision.record tests: static
    /// client-asserted labels, one correlation id per call site.
    fn chain_for_test() -> DelegationChain {
        DelegationChain {
            correlation_id: OperationId::from_string(String::from("op_decision_record_test")),
            parent_op_id: None,
            principals: vec![String::from("user:test")],
        }
    }

    /// The `#[tool_router]`/`#[tool_handler]` macros expand for the
    /// gateway and the router-disable path keeps `ServerHandler` intact.
    #[test]
    fn rmcp_tool_macro_compiles_for_gateway() -> McpGatewayResult<()> {
        /// Compile-time proof that the gateway implements `ServerHandler`.
        fn assert_server_handler<T: ServerHandler>() {}
        assert_server_handler::<McpGateway>();

        let temp = tempfile::tempdir()?;
        let scope = ProjectScope::from_root(temp.path())?;
        let gateway = McpGateway::new(GatewayContext {
            scope,
            servers: ResolvedMcpServers::default(),
            arbitraitor: ArbitraitorClient::default(),
            board: None,
            board_audit_store: None,
            decision_store: None,
            board_lease_registry: None,
            pr_status: None,
            pr_status_repo_root: None,
            pr_status_allowed_repo: None,
        });
        let _ = gateway;
        Ok(())
    }

    /// F2 (issue #458): with no board provider, `board.query` is DISABLED —
    /// hidden from the router's tool list AND rejected by call. Probed
    /// through the same `disable_route` path rmcp's `list_all`/`get`/`call`
    /// honor (verified against rmcp 3.5.1 source).
    #[test]
    fn board_query_is_absent_from_tool_list_when_unconfigured() -> McpGatewayResult<()> {
        let temp = tempfile::tempdir()?;
        let scope = ProjectScope::from_root(temp.path())?;
        let unconfigured = McpGateway::new(GatewayContext {
            scope,
            servers: ResolvedMcpServers::default(),
            arbitraitor: ArbitraitorClient::default(),
            board: None,
            board_audit_store: None,
            decision_store: None,
            board_lease_registry: None,
            pr_status: None,
            pr_status_repo_root: None,
            pr_status_allowed_repo: None,
        });
        let listed: Vec<String> = unconfigured
            .tool_router
            .list_all()
            .iter()
            .map(|tool| tool.name.to_string())
            .collect();
        assert!(
            !listed.iter().any(|name| name == "board.query"),
            "board.query must be hidden from tools/list when unconfigured: {listed:?}"
        );
        assert!(
            !unconfigured.tool_router.has_route("board.query"),
            "board.query must be uncallsable when unconfigured"
        );
        assert!(
            unconfigured.tool_router.is_disabled("board.query"),
            "the route is disabled, not removed"
        );
        // board.move disables alongside board.query: the guard surface
        // never runs against a partial configuration (fail closed).
        assert!(
            !unconfigured.tool_router.has_route("board.move"),
            "board.move must be uncallsable when unconfigured"
        );
        assert!(unconfigured.tool_router.is_disabled("board.move"));
        Ok(())
    }

    /// Call-path counterpart of the disabled-route probe: a `tools/call`
    /// for an unconfigured route must return the router's typed
    /// "tool not found" invalid-params error — rmcp 3.5.1's
    /// `ToolRouter::call` checks the disabled set before consulting the
    /// route map. A regression that keeps `list_all`/`has_route`/
    /// `is_disabled` intact but silently accepts `call` fails this
    /// assertion.
    #[tokio::test]
    async fn disabled_board_query_call_is_rejected_as_tool_not_found() {
        use rmcp::handler::server::tool::ToolCallContext;
        use rmcp::model::{CallToolRequestParams, NumberOrString};
        use rmcp::service::RequestContext;
        use std::borrow::Cow;

        let temp = tempfile::tempdir().unwrap();
        let scope = ProjectScope::from_root(temp.path()).unwrap();
        let unconfigured = McpGateway::new(GatewayContext {
            scope,
            servers: ResolvedMcpServers::default(),
            arbitraitor: ArbitraitorClient::default(),
            board: None,
            board_audit_store: None,
            decision_store: None,
            board_lease_registry: None,
            pr_status: None,
            pr_status_repo_root: None,
            pr_status_allowed_repo: None,
        });
        assert!(unconfigured.tool_router.is_disabled("board.query"));

        // Drive the same dispatch the macro-generated `call_tool` uses:
        // `ToolCallContext::new(self, params, context)` ->
        // `ToolRouter::call`. The peer is never reached on the
        // disabled-route rejection path, but a live `Peer` keeps the
        // context well-formed; the transport is an in-memory duplex whose
        // client side is dropped unread.
        let request = CallToolRequestParams::new(Cow::Borrowed("board.query")).with_arguments(
            serde_json::json!({})
                .as_object()
                .cloned()
                .unwrap_or_default(),
        );
        let context = RequestContext::<rmcp::RoleServer>::new(
            NumberOrString::Number(1),
            unused_server_peer(),
        );
        let call = unconfigured
            .tool_router
            .call(ToolCallContext::new(&unconfigured, request, context))
            .await;
        match call {
            Ok(_) => panic!("disabled board.query route must reject, not succeed"),
            Err(error) => {
                assert_eq!(
                    error.code,
                    rmcp::model::ErrorCode::INVALID_PARAMS,
                    "disabled route rejection must be the router's typed error: {error:?}"
                );
                assert_eq!(error.message, "tool not found");
            }
        }
    }

    /// Test-only `Peer<RoleServer>` over an in-memory duplex transport:
    /// `Peer::new` is `pub(crate)` in rmcp, so the peer is built by
    /// `serve_directly`-ing a no-op handler over a duplex stream whose
    /// client side is dropped unread. The peer is never used by the
    /// disabled-route rejection path; it only makes the request context
    /// well-formed.
    fn unused_server_peer() -> rmcp::Peer<rmcp::RoleServer> {
        use rmcp::service::{RunningService, serve_directly};
        struct NullHandler;
        impl rmcp::ServerHandler for NullHandler {
            fn get_info(&self) -> rmcp::model::ServerConfig {
                rmcp::model::ServerConfig::new(rmcp::model::ServerCapabilities::default())
            }
        }
        let (_server_io, client_io) = tokio::io::duplex(4096);
        let running: RunningService<rmcp::RoleServer, NullHandler> =
            serve_directly(NullHandler, client_io, None);
        let peer = running.peer().clone();
        tokio::spawn(async move {
            running.cancel().await.ok();
        });
        peer
    }

    /// A provider WITHOUT a lease registry also disables both board tools:
    /// `board.move` needs both, and a partial guard configuration fails
    /// closed rather than running unlease-checked.
    #[test]
    fn board_tools_disable_when_lease_registry_is_missing() -> McpGatewayResult<()> {
        let temp = tempfile::tempdir()?;
        let scope = ProjectScope::from_root(temp.path())?;
        let partial = McpGateway::new(GatewayContext {
            scope,
            servers: ResolvedMcpServers::default(),
            arbitraitor: ArbitraitorClient::default(),
            board: Some(std::sync::Arc::new(
                orchestraitor_board_contract::InMemoryBoardProvider::new(|_| {}),
            )),
            board_audit_store: None,
            decision_store: None,
            board_lease_registry: None,
            pr_status: None,
            pr_status_repo_root: None,
            pr_status_allowed_repo: None,
        });
        assert!(!partial.tool_router.has_route("board.move"));
        assert!(partial.tool_router.is_disabled("board.move"));
        assert!(!partial.tool_router.has_route("board.query"));
        Ok(())
    }

    /// F2 counterpart: WITH a board provider, `board.query` is listed and
    /// callable.
    #[test]
    fn board_query_is_listed_when_a_provider_is_configured() -> McpGatewayResult<()> {
        let temp = tempfile::tempdir()?;
        let scope = ProjectScope::from_root(temp.path())?;
        let configured = McpGateway::new(GatewayContext {
            scope,
            servers: ResolvedMcpServers::default(),
            arbitraitor: ArbitraitorClient::default(),
            board: Some(std::sync::Arc::new(
                orchestraitor_board_contract::InMemoryBoardProvider::new(|_| {}),
            )),
            board_audit_store: None,
            decision_store: None,
            board_lease_registry: Some(std::sync::Arc::new(std::sync::Mutex::new(
                crate::board_move::InMemoryLeaseRegistry::new(),
            ))),
            pr_status: None,
            pr_status_repo_root: None,
            pr_status_allowed_repo: None,
        });
        let listed: Vec<String> = configured
            .tool_router
            .list_all()
            .iter()
            .map(|tool| tool.name.to_string())
            .collect();
        assert!(
            listed.iter().any(|name| name == "board.query"),
            "board.query must be listed when a provider is configured: {listed:?}"
        );
        assert!(configured.tool_router.has_route("board.query"));
        // With provider + lease registry both configured, board.move is
        // listed and callable too.
        assert!(
            listed.iter().any(|name| name == "board.move"),
            "board.move must be listed when fully configured: {listed:?}"
        );
        assert!(configured.tool_router.has_route("board.move"));
        Ok(())
    }

    /// `decision.record` (issue #334) is listed and callable exactly when
    /// a session-scoped decision store is configured: with one, the route
    /// is enabled; without one the route is DISABLED (hidden from
    /// `tools/list`, calls rejected) and the shared runner refuses with the
    /// typed `decision_record_unconfigured` error — a successful report
    /// must never rest on a store the invocation drops.
    #[test]
    fn decision_record_is_listed_and_callable() -> McpGatewayResult<()> {
        let temp = tempfile::tempdir()?;
        let scope = ProjectScope::from_root(temp.path())?;

        // Without a store: hidden from the list, route disabled.
        let unconfigured = McpGateway::new(GatewayContext {
            scope: scope.clone(),
            servers: ResolvedMcpServers::default(),
            arbitraitor: ArbitraitorClient::default(),
            board: None,
            board_audit_store: None,
            decision_store: None,
            board_lease_registry: None,
            pr_status: None,
            pr_status_repo_root: None,
            pr_status_allowed_repo: None,
        });
        let listed: Vec<String> = unconfigured
            .tool_router
            .list_all()
            .iter()
            .map(|tool| tool.name.to_string())
            .collect();
        assert!(
            !listed.iter().any(|name| name == "decision.record"),
            "decision.record must be hidden without a decision store: {listed:?}"
        );
        assert!(
            !unconfigured.tool_router.has_route("decision.record"),
            "the route must be disabled without a decision store"
        );

        // With a store: listed and callable.
        let shared = std::sync::Arc::new(std::sync::Mutex::new(
            orchestraitor_campaign::CampaignDecisionStore::open_in_memory()
                .map_err(|error| McpGatewayError::DecisionRecord(error.to_string()))?,
        ));
        let configured = McpGateway::new(GatewayContext {
            scope,
            servers: ResolvedMcpServers::default(),
            arbitraitor: ArbitraitorClient::default(),
            board: None,
            board_audit_store: None,
            decision_store: Some(shared.clone()),
            board_lease_registry: None,
            pr_status: None,
            pr_status_repo_root: None,
            pr_status_allowed_repo: None,
        });
        let listed: Vec<String> = configured
            .tool_router
            .list_all()
            .iter()
            .map(|tool| tool.name.to_string())
            .collect();
        assert!(
            listed.iter().any(|name| name == "decision.record"),
            "decision.record must be listed when a decision store is configured: {listed:?}"
        );
        assert!(configured.tool_router.has_route("decision.record"));
        Ok(())
    }

    /// The typed unconfigured refusal (PR #479 review, major): with no
    /// session-scoped decision store, the shared runner REFUSES with
    /// `decision_record_unconfigured` instead of opening a per-invocation
    /// in-memory store and reporting success for an append it drops.
    #[test]
    fn decision_record_without_store_is_refused_unconfigured() -> McpGatewayResult<()> {
        let outcome = run_decision_record_shared(
            &crate::decision_record::DecisionRecordInput {
                kind: crate::decision_record::DecisionRecordKind::NoOp,
                no_op_reason: Some(crate::decision_record::DecisionRecordNoOpReason::EmptyQueue),
                selected: None,
                role: String::from("implement"),
                provider: String::from("neuralwatt"),
                model: String::from("glm-5.2"),
                precedence_path: String::from("bootstrap-default"),
                fallback_reason: None,
                worker_args: Vec::new(),
                rationale: String::from("unconfigured store probe"),
                alternatives: Vec::new(),
                blocked_graph: Vec::new(),
                skipped: Vec::new(),
            },
            &chain_for_test(),
            None,
            None,
        );
        match outcome {
            Err(McpGatewayError::DecisionRecordUnconfigured) => Ok(()),
            other => Err(McpGatewayError::DecisionRecord(format!(
                "expected DecisionRecordUnconfigured, got {other:?}"
            ))),
        }
    }

    /// `decision.record` end to end through the shared runner (issue #334):
    /// two invocations against ONE session-scoped store append two rows with
    /// strictly increasing ids and identical payloads, and the original row
    /// is byte-identical after the second append — the forbidden effect (a
    /// mutation of stored state) did not happen.
    #[test]
    fn decision_record_session_store_appends_without_mutating() -> McpGatewayResult<()> {
        use crate::decision_record::{DecisionRecordInput, DecisionRecordKind};
        let temp = tempfile::tempdir()?;
        let scope = ProjectScope::from_root(temp.path())?;
        let shared = std::sync::Arc::new(std::sync::Mutex::new(
            orchestraitor_campaign::CampaignDecisionStore::open_in_memory()
                .map_err(|error| McpGatewayError::DecisionRecord(error.to_string()))?,
        ));
        let _gateway = McpGateway::new(GatewayContext {
            scope,
            servers: ResolvedMcpServers::default(),
            arbitraitor: ArbitraitorClient::default(),
            board: None,
            board_audit_store: None,
            decision_store: Some(shared.clone()),
            board_lease_registry: None,
            pr_status: None,
            pr_status_repo_root: None,
            pr_status_allowed_repo: None,
        });
        let input = DecisionRecordInput {
            kind: DecisionRecordKind::NoOp,
            no_op_reason: Some(crate::decision_record::DecisionRecordNoOpReason::EmptyQueue),
            selected: None,
            role: String::from("implement"),
            provider: String::from("neuralwatt"),
            model: String::from("glm-5.2"),
            precedence_path: String::from("bootstrap-default"),
            fallback_reason: None,
            worker_args: Vec::new(),
            rationale: String::from("session-scoped append probe"),
            alternatives: Vec::new(),
            blocked_graph: Vec::new(),
            skipped: Vec::new(),
        };
        let chain = DelegationChain {
            correlation_id: OperationId::new(),
            parent_op_id: None,
            principals: vec![String::from("user:test")],
        };
        let first = run_decision_record_shared(&input, &chain, Some(&shared), None)
            .map_err(|error| McpGatewayError::DecisionRecord(error.to_string()))?;
        let second = run_decision_record_shared(&input, &chain, Some(&shared), None)
            .map_err(|error| McpGatewayError::DecisionRecord(error.to_string()))?;
        assert!(
            second.id > first.id,
            "append-only: ids strictly increase across session invocations"
        );
        assert_eq!(
            first.decision, second.decision,
            "identical input persists an identical payload"
        );
        let listed = shared
            .lock()
            .map_err(|_| McpGatewayError::DecisionRecord(String::from("store poisoned")))?
            .list()
            .map_err(|error| McpGatewayError::DecisionRecord(error.to_string()))?;
        assert_eq!(listed.len(), 2, "both rows replay; nothing was replaced");
        assert_eq!(
            listed[0].decision, first.decision,
            "the original row is untouched after the second append"
        );
        Ok(())
    }

    /// §9.25.1 with a SHARED audit store (issue #476 review F1): a
    /// pre-existing seed event survives, the decision invocation's
    /// `ToolRequest` event CONTINUES the shared hash chain (seq = seed+1,
    /// `prev_hash` = seed's hash), and the whole merged chain validates —
    /// the invocation event is not dropped with a per-call store.
    #[test]
    fn decision_record_extends_shared_audit_chain() -> McpGatewayResult<()> {
        use orchestraitor_events::{
            AuditStore, CURRENT_SCHEMA_VERSION, EventCategory, EventEnvelope, EventEnvelopeInput,
        };
        let _temp = tempfile::tempdir()?;
        let decision_store = std::sync::Arc::new(std::sync::Mutex::new(
            orchestraitor_campaign::CampaignDecisionStore::open_in_memory()
                .map_err(|error| McpGatewayError::DecisionRecord(error.to_string()))?,
        ));
        let shared_audit = std::sync::Arc::new(std::sync::Mutex::new(
            orchestraitor_events::InMemoryAuditStore::default(),
        ));

        // Seed: one pre-existing non-decision.record event.
        let seed = EventEnvelope::try_new(EventEnvelopeInput {
            schema_version: CURRENT_SCHEMA_VERSION,
            monotonic_seq: 1,
            wall_clock_ts: String::from("2026-07-30T00:00:00Z"),
            correlation_id: OperationId::from_string(String::from("op_seed")),
            parent_op_id: None,
            category: EventCategory::SessionLifecycle,
            payload: serde_json::json!({"state": "seeded"}),
            prev_hash: None,
        })
        .map_err(|error| McpGatewayError::DecisionRecord(error.to_string()))?;
        shared_audit
            .lock()
            .map_err(|_| McpGatewayError::DecisionRecord(String::from("seed lock poisoned")))?
            .append(seed)
            .map_err(|error| McpGatewayError::DecisionRecord(error.to_string()))?;

        let input = crate::decision_record::DecisionRecordInput {
            kind: crate::decision_record::DecisionRecordKind::NoOp,
            no_op_reason: Some(crate::decision_record::DecisionRecordNoOpReason::EmptyQueue),
            selected: None,
            role: String::from("implement"),
            provider: String::from("neuralwatt"),
            model: String::from("glm-5.2"),
            precedence_path: String::from("bootstrap-default"),
            fallback_reason: None,
            worker_args: Vec::new(),
            rationale: String::from("shared-audit probe"),
            alternatives: Vec::new(),
            blocked_graph: Vec::new(),
            skipped: Vec::new(),
        };
        let chain = DelegationChain {
            correlation_id: OperationId::new(),
            parent_op_id: None,
            principals: vec![String::from("user:test")],
        };
        run_decision_record_shared(&input, &chain, Some(&decision_store), Some(&shared_audit))
            .map_err(|error| McpGatewayError::DecisionRecord(error.to_string()))?;

        let records = shared_audit
            .lock()
            .map_err(|_| McpGatewayError::DecisionRecord(String::from("audit lock poisoned")))?
            .records()
            .to_vec();
        assert_eq!(
            records.len(),
            2,
            "seed + one ToolRequest per invocation; the seed survives"
        );
        assert_eq!(records[0].envelope.payload["state"], "seeded");
        assert_eq!(
            records[1].envelope.payload["tool"],
            crate::decision_record::TOOL_NAME,
            "the invocation event persisted into the shared store"
        );
        orchestraitor_events::validate_hash_chain(&records)
            .map_err(|error| McpGatewayError::DecisionRecord(error.to_string()))?;
        Ok(())
    }

    /// Review F2 (PR #476 gen-2): two concurrent invocations against ONE
    /// shared audit store — the lock is held across each invocation's whole
    /// event append, so both `ToolRequest` events survive with distinct
    /// sequence numbers, the seed is intact, and the merged chain validates
    /// (no snapshot/replay lost-update window).
    #[test]
    fn decision_record_concurrent_shared_audit_appends_serialize() -> McpGatewayResult<()> {
        use crate::decision_record::{DecisionRecordKind, DecisionRecordNoOpReason};
        use orchestraitor_events::{
            AuditStore, CURRENT_SCHEMA_VERSION, EventCategory, EventEnvelope, EventEnvelopeInput,
        };
        use std::sync::Arc;

        let shared_audit = Arc::new(std::sync::Mutex::new(
            orchestraitor_events::InMemoryAuditStore::default(),
        ));

        // Seed: one pre-existing non-decision.record event.
        let seed = EventEnvelope::try_new(EventEnvelopeInput {
            schema_version: CURRENT_SCHEMA_VERSION,
            monotonic_seq: 1,
            wall_clock_ts: String::from("2026-07-30T00:00:00Z"),
            correlation_id: orchestraitor_model::OperationId::from_string(String::from("op_seed")),
            parent_op_id: None,
            category: EventCategory::SessionLifecycle,
            payload: serde_json::json!({"state": "seeded"}),
            prev_hash: None,
        })
        .unwrap();
        shared_audit
            .lock()
            .map_err(|_| String::from("seed lock poisoned"))
            .and_then(|mut store| {
                store
                    .append(seed)
                    .map_err(|error| format!("seed append failed: {error}"))
            })
            .unwrap();

        let make_input = || crate::decision_record::DecisionRecordInput {
            kind: DecisionRecordKind::NoOp,
            no_op_reason: Some(DecisionRecordNoOpReason::EmptyQueue),
            selected: None,
            role: String::from("implement"),
            provider: String::from("neuralwatt"),
            model: String::from("glm-5.2"),
            precedence_path: String::from("bootstrap-default"),
            fallback_reason: None,
            worker_args: Vec::new(),
            rationale: String::from("concurrent shared-audit probe"),
            alternatives: Vec::new(),
            blocked_graph: Vec::new(),
            skipped: Vec::new(),
        };
        // Real OS-thread stress (review round 2): `tokio::join!` over
        // synchronous bodies does NOT overlap them — the first call runs to
        // completion before the second is polled, so a snapshot/write-back
        // implementation could pass spuriously. N threads × M invocations
        // against one shared audit store, joined at thread boundaries,
        // genuinely interleave; under the old lock scope the lost update
        // would surface as a broken hash chain or a missing event.
        // The session-scoped decision store is REQUIRED (review round 4,
        // PR #479): the invocations append through one shared store.
        let shared_decisions = Arc::new(std::sync::Mutex::new(
            orchestraitor_campaign::CampaignDecisionStore::open_in_memory()
                .map_err(|error| McpGatewayError::DecisionRecord(error.to_string()))?,
        ));
        let (threads, invocations_per_thread) = (8usize, 6usize);
        let mut handles = Vec::new();
        for _ in 0..threads {
            let shared_audit = shared_audit.clone();
            let shared_decisions = shared_decisions.clone();
            handles.push(std::thread::spawn(move || {
                for _ in 0..invocations_per_thread {
                    let input = make_input();
                    run_decision_record_shared(
                        &input,
                        &chain_for_test(),
                        Some(&shared_decisions),
                        Some(&shared_audit),
                    )
                    .expect("concurrent invocation succeeds");
                }
            }));
        }
        for handle in handles {
            handle.join().expect("stress thread completes");
        }

        let records = shared_audit
            .lock()
            .map_err(|_| String::from("final lock poisoned"))
            .map(|store| store.records().to_vec())
            .unwrap();
        assert_eq!(
            records.len(),
            1 + threads * invocations_per_thread,
            "seed + one ToolRequest per invocation, nothing lost"
        );
        assert_eq!(records[0].envelope.payload["state"], "seeded");
        assert_eq!(
            records[1].envelope.payload["tool"],
            crate::decision_record::TOOL_NAME
        );
        assert_eq!(
            records[2].envelope.payload["tool"],
            crate::decision_record::TOOL_NAME
        );
        let sequences: Vec<_> = records
            .iter()
            .map(|record| record.envelope.monotonic_seq)
            .collect();
        let mut sorted = sequences.clone();
        sorted.sort_unstable();
        assert_eq!(
            sequences, sorted,
            "hash chain continues in sequence order: no lost update"
        );
        orchestraitor_events::validate_hash_chain(&records)
            .map_err(|error| McpGatewayError::DecisionRecord(error.to_string()))?;
        Ok(())
    }

    /// N2 (issue #458-gen2): a shared store with a PRE-EXISTING seed event
    /// survives two concurrent invocations — final count = seed + 2, every
    /// original event intact, and the whole chain validates. The N1 probe
    /// (one-at-a-time import of a multi-record history) is structurally
    /// gone: the shared path now appends under one lock section and never
    /// calls import at all.
    #[tokio::test]
    async fn shared_store_survives_concurrent_invocations_with_seed_events() {
        use orchestraitor_board_contract::{BoardItemType, InMemoryBoardProvider};
        use orchestraitor_events::{
            AuditStore, CURRENT_SCHEMA_VERSION, EventCategory, EventEnvelope, EventEnvelopeInput,
        };
        use std::sync::Arc;

        use crate::board_query::BoardQueryFilter;

        let board = Arc::new(InMemoryBoardProvider::new(|setup| {
            setup.item("only", BoardItemType::Task, "Only item", "b", "Ready", &[]);
        }));
        let shared = Arc::new(std::sync::Mutex::new(
            orchestraitor_events::InMemoryAuditStore::default(),
        ));

        // Seed: one pre-existing non-board.query event. Errors are mapped
        // to messages (no unwrap): the test fails loudly either way.
        let seed = EventEnvelope::try_new(EventEnvelopeInput {
            schema_version: CURRENT_SCHEMA_VERSION,
            monotonic_seq: 1,
            wall_clock_ts: String::from("2026-07-30T00:00:00Z"),
            correlation_id: orchestraitor_model::OperationId::from_string(String::from("op_seed")),
            parent_op_id: None,
            category: EventCategory::SessionLifecycle,
            payload: serde_json::json!({"state": "seeded"}),
            prev_hash: None,
        })
        .unwrap();
        let seed_result = shared
            .lock()
            .map_err(|_| String::from("seed lock poisoned"))
            .and_then(|mut store| {
                store
                    .append(seed)
                    .map_err(|error| format!("seed append failed: {error}"))
            });
        seed_result.unwrap();

        let run = || {
            let board = board.clone();
            let shared = shared.clone();
            async move {
                // Direct call into the crate-visible shared-store path.
                crate::gateway::run_board_query_shared(
                    board.as_ref(),
                    &BoardQueryMode::Search {
                        filter: BoardQueryFilter::default(),
                    },
                    vec![String::from("user:test")],
                    Some(shared),
                )
                .await
            }
        };
        let (first, second) = tokio::join!(run(), run());
        first.expect("first invocation succeeds");
        second.expect("second invocation succeeds");

        let records = shared
            .lock()
            .map_err(|_| String::from("final lock poisoned"))
            .map(|store| store.records().to_vec())
            .unwrap();
        assert_eq!(
            records.len(),
            3,
            "seed + one ToolRequest per invocation, nothing lost"
        );
        assert_eq!(
            records[0].envelope.payload["state"], "seeded",
            "seed event intact"
        );
        assert_eq!(records[1].envelope.payload["tool"], "board.query");
        assert_eq!(records[2].envelope.payload["tool"], "board.query");
        orchestraitor_events::validate_hash_chain(&records)
            .expect("merged chain validates end to end");
    }

    // ------------------------------------------------------------------
    // `pr.status` (spec §9.39, §9.48; issue #614 slice 2)
    // ------------------------------------------------------------------

    /// A stub §9.48 transport: replays queued GraphQL payloads in order.
    /// Each queued payload is the full HTTP body (the same shape the CLI's
    /// e2e harness serves).
    struct StubTransport {
        payloads: std::sync::Mutex<std::vec::IntoIter<String>>,
    }

    impl StubTransport {
        fn new(payloads: Vec<String>) -> Self {
            Self {
                payloads: std::sync::Mutex::new(payloads.into_iter()),
            }
        }
    }

    impl crate::pr_status::PrStatusTransport for StubTransport {
        fn graphql(
            &self,
            _operation: &str,
            _query: &str,
            _variables: &serde_json::Value,
        ) -> Result<(serde_json::Value, bool), crate::pr_status::PrStatusError> {
            let next = self
                .payloads
                .lock()
                .map_err(|_| crate::pr_status::PrStatusError::Unrecoverable {
                    message: String::from("stub transport poisoned"),
                    partial_json: String::new(),
                })?
                .next()
                .unwrap_or_else(|| String::from("{\"data\":{}}"));
            let envelope: serde_json::Value =
                serde_json::from_str(&next).expect("stub payload is valid JSON");
            let had_errors = envelope
                .get("errors")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|errors| !errors.is_empty());
            let data = envelope
                .get("data")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            Ok((data, had_errors))
        }
    }

    /// Builds one mergeable GraphQL payload set (the same order the engine
    /// issues operations: pr view, checks, threads, body, post-read view).
    fn mergeable_payloads() -> Vec<String> {
        let view = serde_json::json!({
            "data": {"repository": {"pullRequest": {
                "number": 614, "state": "OPEN",
                "headRefOid": "cb7cf776939f65981f9fb7c9e5263ba0b20b7c7c",
                "baseRefOid": "9cc67bf0db85a7248a9b9718974f4460d5fb3759",
                "isDraft": false, "reviewDecision": "APPROVED",
                "mergeStateStatus": "CLEAN"
            }}}
        });
        let checks = serde_json::json!({
            "data": {"repository": {"object": {"statusCheckRollup": {
                "contexts": {"pageInfo": {"hasNextPage": false}, "nodes": [
                    {"__typename": "CheckRun", "name": "cool tests",
                     "status": "COMPLETED", "conclusion": "SUCCESS",
                     "detailsUrl": "sweet link",
                     "checkSuite": {"workflowRun": {"workflow": {"name": "code.yml"}}}}
                ]}
            }}}}
        });
        let threads = serde_json::json!({
            "data": {"repository": {"pullRequest": {"reviewThreads": {
                "pageInfo": {"hasNextPage": false}, "nodes": []
            }}}}
        });
        let body = serde_json::json!({
            "data": {"repository": {"pullRequest": {"body": "no markers"}}}
        });
        vec![
            view.to_string(),
            checks.to_string(),
            threads.to_string(),
            body.to_string(),
            // The post-read anchor check repeats the PR view.
            view.to_string(),
        ]
    }

    fn pr_status_request(number: u64) -> PrStatusRequest {
        PrStatusRequest {
            number,
            owner: String::from("arbsec"),
            repo: String::from("orchestraitor"),
            delegation_chain: vec![String::from("user:test")],
        }
    }

    fn pr_status_gateway(
        transport: Option<std::sync::Arc<dyn crate::pr_status::PrStatusTransport>>,
        shared: Option<std::sync::Arc<std::sync::Mutex<orchestraitor_events::InMemoryAuditStore>>>,
    ) -> (McpGateway, std::path::PathBuf, tempfile::TempDir) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();
        let scope = ProjectScope::from_root(&root).unwrap();
        let gateway = McpGateway::new(GatewayContext {
            scope,
            servers: ResolvedMcpServers::default(),
            arbitraitor: ArbitraitorClient::default(),
            board: None,
            board_audit_store: shared,
            decision_store: None,
            board_lease_registry: None,
            pr_status: transport,
            pr_status_repo_root: Some(root.clone()),
            pr_status_allowed_repo: Some(String::from("arbsec/orchestraitor")),
        });
        (gateway, root, temp)
    }

    /// (a) With no transport + CLEAN-record root, `pr.status` is DISABLED:
    /// hidden from `tools/list` and rejected by call — the same
    /// `disable_route` posture as `board.query`.
    #[test]
    fn pr_status_is_absent_from_tool_list_when_unconfigured() -> McpGatewayResult<()> {
        let temp = tempfile::tempdir()?;
        let scope = ProjectScope::from_root(temp.path())?;
        let unconfigured = McpGateway::new(GatewayContext {
            scope,
            servers: ResolvedMcpServers::default(),
            arbitraitor: ArbitraitorClient::default(),
            board: None,
            board_audit_store: None,
            decision_store: None,
            board_lease_registry: None,
            pr_status: None,
            pr_status_repo_root: None,
            pr_status_allowed_repo: None,
        });
        let listed: Vec<String> = unconfigured
            .tool_router
            .list_all()
            .iter()
            .map(|tool| tool.name.to_string())
            .collect();
        assert!(
            !listed.iter().any(|name| name == "pr.status"),
            "pr.status must be hidden from tools/list when unconfigured: {listed:?}"
        );
        assert!(!unconfigured.tool_router.has_route("pr.status"));
        assert!(unconfigured.tool_router.is_disabled("pr.status"));
        Ok(())
    }

    /// (a2) A transport WITHOUT a CLEAN-record root also disables the
    /// route: the tool never runs against a partial configuration.
    #[test]
    fn pr_status_is_disabled_without_repo_root() -> McpGatewayResult<()> {
        let temp = tempfile::tempdir()?;
        let scope = ProjectScope::from_root(temp.path())?;
        let partial = McpGateway::new(GatewayContext {
            scope,
            servers: ResolvedMcpServers::default(),
            arbitraitor: ArbitraitorClient::default(),
            board: None,
            board_audit_store: None,
            decision_store: None,
            board_lease_registry: None,
            pr_status: Some(std::sync::Arc::new(StubTransport::new(Vec::new()))),
            pr_status_repo_root: None,
            pr_status_allowed_repo: None,
        });
        assert!(!partial.tool_router.has_route("pr.status"));
        assert!(partial.tool_router.is_disabled("pr.status"));
        Ok(())
    }

    /// (a3) A transport + repo root WITHOUT the allowed repository ALSO
    /// disables the route: all three context values are required — an
    /// operator who sets only two gets a hidden tool, never a partially
    /// scoped one.
    #[test]
    fn pr_status_is_disabled_without_allowed_repo() -> McpGatewayResult<()> {
        let temp = tempfile::tempdir()?;
        let scope = ProjectScope::from_root(temp.path())?;
        let partial = McpGateway::new(GatewayContext {
            scope,
            servers: ResolvedMcpServers::default(),
            arbitraitor: ArbitraitorClient::default(),
            board: None,
            board_audit_store: None,
            decision_store: None,
            board_lease_registry: None,
            pr_status: Some(std::sync::Arc::new(StubTransport::new(Vec::new()))),
            pr_status_repo_root: Some(temp.path().to_path_buf()),
            pr_status_allowed_repo: None,
        });
        assert!(!partial.tool_router.has_route("pr.status"));
        assert!(partial.tool_router.is_disabled("pr.status"));
        Ok(())
    }

    /// (e) An out-of-scope request (owner/repo outside the configured
    /// slug) is refused with the typed `pr_status_out_of_scope` payload
    /// BEFORE the engine runs, AND the refusal is recorded in the audit
    /// store with the `out_of_scope` outcome class — scope-boundary probes
    /// are the events an auditor needs most.
    #[tokio::test]
    async fn pr_status_out_of_scope_request_is_refused_and_recorded() {
        let shared = std::sync::Arc::new(std::sync::Mutex::new(
            orchestraitor_events::InMemoryAuditStore::default(),
        ));
        let (gateway, _root, _temp) = pr_status_gateway(
            Some(std::sync::Arc::new(StubTransport::new(Vec::new()))),
            Some(shared.clone()),
        );

        let result = gateway
            .pr_status_tool(Parameters(PrStatusRequest {
                number: 1,
                owner: String::from("evil"),
                repo: String::from("scope-probe"),
                delegation_chain: vec![String::from("user:test")],
            }))
            .await
            .expect("tool call returns a structured result");

        // The refusal is the typed payload, never prose-only, and names the
        // configured scope — it does NOT echo the refused values as
        // authority.
        let text = result
            .content
            .first()
            .and_then(|content| content.as_text().map(|text| text.text.clone()))
            .unwrap_or_default();
        assert!(text.contains("pr_status_out_of_scope"), "{text}");
        assert!(text.contains("arbsec/orchestraitor"), "{text}");

        // The refusal is recorded with the out_of_scope outcome class.
        let records = shared
            .lock()
            .map_err(|_| String::from("audit lock poisoned"))
            .map(|store| store.records().to_vec())
            .unwrap();
        assert_eq!(records.len(), 1, "one refusal event recorded");
        assert_eq!(records[0].envelope.payload["tool"], "pr.status");
        assert_eq!(
            records[0].envelope.payload["result_summary"]["outcome"],
            "out_of_scope"
        );
        assert_eq!(
            records[0].envelope.payload["result_summary"]["allowed"],
            "arbsec/orchestraitor"
        );
        assert_eq!(records[0].envelope.payload["request"]["owner"], "evil");
        orchestraitor_events::validate_hash_chain(&records)
            .expect("the refusal event continues the shared chain");
    }

    /// (b) Configured happy path: a stub transport serving a mergeable
    /// fixture produces the typed §9.48 structured content AND records a
    /// `ToolRequest` event with tool name `pr.status` that continues the
    /// shared audit chain.
    #[tokio::test]
    async fn pr_status_happy_path_returns_typed_verdict_and_records_event() {
        let shared = std::sync::Arc::new(std::sync::Mutex::new(
            orchestraitor_events::InMemoryAuditStore::default(),
        ));
        let (_gateway, root, _temp) = pr_status_gateway(None, Some(shared.clone()));

        // A CLEAN review record at the head: the convergence gate requires
        // it for a mergeable verdict.
        let records_dir = root.join(".orchestraitor").join("reviews");
        std::fs::create_dir_all(&records_dir).unwrap();
        std::fs::write(
            records_dir.join("614-cb7cf77.md"),
            "# Agent review record: PR #614 @ cb7cf77\n\n\
             - verdict: CLEAN\n\
             - reviewer-agent: test\n\
             - head SHA: cb7cf776939f65981f9fb7c9e5263ba0b20b7c7c\n\
             - base OID: 9cc67bf0db85a7248a9b9718974f4460d5fb3759\n\
             - findings: 0\n\nNo findings.\n",
        )
        .unwrap();

        let verdict = run_pr_status_shared(
            std::sync::Arc::new(StubTransport::new(mergeable_payloads())),
            root,
            pr_status_request(614),
            Some(shared.clone()),
        )
        .await
        .expect("aggregate succeeds");

        assert!(verdict.convergence.mergeable);
        assert_eq!(verdict.number, 614);
        assert_eq!(verdict.repository, "arbsec/orchestraitor");

        let records = shared
            .lock()
            .map_err(|_| String::from("audit lock poisoned"))
            .map(|store| store.records().to_vec())
            .unwrap();
        assert_eq!(records.len(), 1, "one ToolRequest event per invocation");
        assert_eq!(records[0].envelope.payload["tool"], "pr.status");
        assert_eq!(records[0].envelope.payload["request"]["number"], 614);
        assert_eq!(
            records[0].envelope.payload["result_summary"]["mergeable"],
            true
        );
        orchestraitor_events::validate_hash_chain(&records)
            .expect("the pr.status event continues the shared chain");
    }

    /// (c) Mediation/refusal negative: the checks/threads/body sub-reads
    /// fail closed (unread sub-checks) while the anchor check passes — the
    /// invocation surfaces the TYPED error payload with code
    /// `pr_status_unrecoverable` and the degraded partial verdict (never
    /// prose-only), and the failed read still records its event.
    #[tokio::test]
    async fn pr_status_unrecoverable_transport_returns_typed_payload_with_partial_verdict() {
        // The view replays for EVERY graphql call the engine makes: the
        // checks/threads/body reads then fail closed on their pointer
        // misses (unread sub-checks), and the post-read anchor check
        // PASSES (same view) so the verdict assembly's typed
        // Unrecoverable + partial verdict is what surfaces, not an anchor
        // refusal or a not-found.
        let view = mergeable_payloads()[0].clone();
        let shared = std::sync::Arc::new(std::sync::Mutex::new(
            orchestraitor_events::InMemoryAuditStore::default(),
        ));
        let (_gateway, root, _temp) = pr_status_gateway(None, Some(shared.clone()));

        let outcome = run_pr_status_shared(
            std::sync::Arc::new(StubTransport::new(vec![
                view.clone(),
                view.clone(),
                view.clone(),
                view.clone(),
                view,
            ])),
            root,
            pr_status_request(614),
            Some(shared.clone()),
        )
        .await;

        let tool_error = outcome.expect_err("the unread checks read fails the invocation");
        assert_eq!(tool_error.payload["code"], "pr_status_unrecoverable");
        assert!(
            tool_error.payload.get("partial_verdict").is_some(),
            "the degraded §9.48 verdict rides the error payload: {tool_error:?}"
        );
        assert_eq!(
            tool_error.payload["partial_verdict"]["convergence"]["reasons"][0], "unread(checks)",
            "the stable unread reason is present in the partial verdict"
        );

        let records = shared
            .lock()
            .map_err(|_| String::from("audit lock poisoned"))
            .map(|store| store.records().to_vec())
            .unwrap();
        assert_eq!(
            records.len(),
            1,
            "the failed read still records its outcome (fail closed, never unrecorded)"
        );
        assert_eq!(records[0].envelope.payload["tool"], "pr.status");
    }

    /// (c2) A concurrent-edit anchor movement maps to the typed
    /// `pr_status_concurrent_edit` payload naming the moved anchor.
    #[tokio::test]
    async fn pr_status_concurrent_edit_maps_to_typed_payload() {
        let moved_head = serde_json::json!({
            "data": {"repository": {"pullRequest": {
                "number": 614, "state": "OPEN",
                "headRefOid": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "baseRefOid": "9cc67bf0db85a7248a9b9718974f4460d5fb3759",
                "isDraft": false, "reviewDecision": "APPROVED",
                "mergeStateStatus": "CLEAN"
            }}}
        });
        let payloads = vec![
            mergeable_payloads()[0].clone(),
            mergeable_payloads()[1].clone(),
            mergeable_payloads()[2].clone(),
            mergeable_payloads()[3].clone(),
            moved_head.to_string(),
        ];
        let (_gateway, root, _temp) = pr_status_gateway(None, None);
        let tool_error = run_pr_status_shared(
            std::sync::Arc::new(StubTransport::new(payloads)),
            root,
            pr_status_request(614),
            None,
        )
        .await
        .expect_err("the anchor movement must refuse");
        assert_eq!(tool_error.payload["code"], "pr_status_concurrent_edit");
        assert_eq!(tool_error.payload["anchor"], "head");
        assert_eq!(
            tool_error.payload["before"],
            "cb7cf776939f65981f9fb7c9e5263ba0b20b7c7c"
        );
        assert_eq!(
            tool_error.payload["after"],
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
    }

    /// (d) Oversized owner/repo strings are truncated in the recorded
    /// event payload (mirrors `board_query`'s `MAX_PAYLOAD_VALUE_CHARS`
    /// bound): the full hostile payload never enters the audit record.
    #[tokio::test]
    async fn pr_status_event_truncates_oversized_owner_and_repo() {
        let shared = std::sync::Arc::new(std::sync::Mutex::new(
            orchestraitor_events::InMemoryAuditStore::default(),
        ));
        let (_gateway, root, _temp) = pr_status_gateway(None, Some(shared.clone()));
        let long_owner = "o".repeat(500);
        let long_repo = "r".repeat(500);

        let _ = run_pr_status_shared(
            std::sync::Arc::new(StubTransport::new(Vec::new())),
            root,
            PrStatusRequest {
                number: 614,
                owner: long_owner,
                repo: long_repo,
                delegation_chain: vec![],
            },
            Some(shared.clone()),
        )
        .await;

        let records = shared
            .lock()
            .map_err(|_| String::from("audit lock poisoned"))
            .map(|store| store.records().to_vec())
            .unwrap();
        assert_eq!(records.len(), 1);
        let payload = &records[0].envelope.payload;
        let owner = payload["request"]["owner"].as_str().expect("owner string");
        let repo = payload["request"]["repo"].as_str().expect("repo string");
        let max = crate::board_query::MAX_PAYLOAD_VALUE_CHARS
            + crate::board_query::TRUNCATION_SUFFIX.chars().count();
        assert!(
            owner.chars().count() <= max && repo.chars().count() <= max,
            "oversized owner/repo are truncated in the audit record"
        );
        assert!(
            owner.ends_with(crate::board_query::TRUNCATION_SUFFIX)
                && repo.ends_with(crate::board_query::TRUNCATION_SUFFIX),
            "truncation carries the static marker"
        );
    }
}
