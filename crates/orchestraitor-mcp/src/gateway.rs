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
    /// uncallsable — verified against rmcp 2.2.0, where `list_all`, `get`,
    /// and `call` all honor the disabled set.
    pub board: Option<std::sync::Arc<dyn BoardProvider>>,
    /// Shared audit store for `board.query` invocation events (§9.25.1).
    /// `None` records per-invocation-volatile (validated, then dropped with
    /// the invocation's store); persistence lands with the daemon
    /// event-store wiring (§9.17).
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

    /// The `decision.record` tool name, used by the router-disable path.
    const DECISION_RECORD_NAME: &'static str = "decision.record";

    /// Creates a gateway for a resolved project scope.
    #[must_use]
    pub fn new(context: GatewayContext) -> Self {
        let fs = FileSystemTools::new(context.scope.clone());
        let mut tool_router = Self::static_tool_router();
        if context.board.is_none() {
            tool_router.disable_route(Self::BOARD_QUERY_NAME);
        }
        if context.decision_store.is_none() {
            tool_router.disable_route(Self::DECISION_RECORD_NAME);
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
}

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

    // The session-scoped decision store is REQUIRED (see the function
    // contract): no store means no persistence, and the tool never reports
    // success for an append it is about to drop.
    let Some(shared_store) = shared_store else {
        return Err(McpGatewayError::DecisionRecordUnconfigured);
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
        });
        let _ = gateway;
        Ok(())
    }

    /// F2 (issue #458): with no board provider, `board.query` is DISABLED —
    /// hidden from the router's tool list AND rejected by call. Probed
    /// through the same `disable_route` path rmcp's `list_all`/`get`/`call`
    /// honor (verified against rmcp 2.2.0 source).
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
}
