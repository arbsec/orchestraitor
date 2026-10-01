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

use crate::board_query::{BoardQueryMode, BoardQueryResultKind, DelegationChain, board_query};
use crate::config::ResolvedMcpServers;
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
}

/// rmcp server exposing Orchestraitor built-in tools for one project scope.
#[derive(Clone)]
pub struct McpGateway {
    context: GatewayContext,
    fs: FileSystemTools,
    workflow: WorkflowTools,
    /// Per-instance router: the `#[tool_router]`-generated static set,
    /// with `board.query` disabled when no board provider is configured
    /// (rmcp `disable_route` hides it from `list_all`/`get` and rejects
    /// `call` — the verified per-connection disable path, issue #458 F2).
    tool_router: rmcp::handler::server::router::tool::ToolRouter<Self>,
}

impl McpGateway {
    /// The `board.query` tool name, used by the router-disable path.
    const BOARD_QUERY_NAME: &'static str = "board.query";

    /// Creates a gateway for a resolved project scope.
    #[must_use]
    pub fn new(context: GatewayContext) -> Self {
        let fs = FileSystemTools::new(context.scope.clone());
        let mut tool_router = Self::static_tool_router();
        if context.board.is_none() {
            tool_router.disable_route(Self::BOARD_QUERY_NAME);
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
    fn fs_rename(
        &self,
        Parameters(input): Parameters<RenameInput>,
    ) -> Result<CallToolResult, ErrorData> {
        self.structured(self.fs.rename(&input.from, &input.to))
    }

    #[tool(name = "fs.remove", description = "Remove a project file or directory")]
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
    fn task_run(
        &self,
        Parameters(input): Parameters<WorkflowRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        self.structured(self.workflow.run(WorkflowKind::Task, &input))
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
struct PathInput {
    path: String,
}

#[derive(Debug, Clone, serde::Deserialize, JsonSchema)]
struct SearchInput {
    path: String,
    query: String,
}

#[derive(Debug, Clone, serde::Deserialize, JsonSchema)]
struct CreateInput {
    path: String,
    content: String,
}

#[derive(Debug, Clone, serde::Deserialize, JsonSchema)]
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

/// Drives the typed board query against a provider, recording into the
/// context's audit store when one is shared, otherwise a fresh in-memory
/// store (issue #458 F3).
///
/// With no shared store the recording is PER-INVOCATION-VOLATILE: the event
/// is written and validated, then dropped with the store. Persistence lands
/// with the daemon event-store wiring (§9.17), not in this slice.
///
/// The shared variant works by draining the shared store's existing records
/// under its lock (a short synchronous section — `MutexGuard` is not `Send`,
/// so no guard may cross an await), replaying them into the invocation's
/// store so the hash chain continues, and writing the merged chain back. A
/// poisoned lock fails the invocation closed.
async fn run_board_query_shared(
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
        let existing = {
            let store = shared
                .lock()
                .map_err(|_| McpGatewayError::BoardQuery(String::from("audit store poisoned")))?;
            store.records().to_vec()
        };
        let mut store = orchestraitor_events::InMemoryAuditStore::default();
        for record in existing {
            store
                .r#import(
                    &serde_json_canonicalizer::to_vec(&record)
                        .map_err(|_| {
                            McpGatewayError::BoardQuery(String::from(
                                "audit replay serialization failed",
                            ))
                        })?
                        .into_iter()
                        .chain(std::iter::once(b'\n'))
                        .collect::<Vec<u8>>(),
                )
                .map_err(|_| McpGatewayError::BoardQuery(String::from("audit replay rejected")))?;
        }
        let result = board_query(provider, mode, &chain, &mut store)
            .await
            .map_err(|error| McpGatewayError::BoardQuery(error.to_string()))?;
        {
            let mut guard = shared
                .lock()
                .map_err(|_| McpGatewayError::BoardQuery(String::from("audit store poisoned")))?;
            *guard = store;
        }
        return Ok(result);
    }
    let mut store = orchestraitor_events::InMemoryAuditStore::default();
    board_query(provider, mode, &chain, &mut store)
        .await
        .map_err(|error| McpGatewayError::BoardQuery(error.to_string()))
}

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
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rmcp_tool_macro_compiles_for_gateway() -> McpGatewayResult<()> {
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
}
