//! In-process MCP gateway logic for MVP.

use orchestraitor_arbitraitor_client::ArbitraitorClient;
use orchestraitor_board_contract::BoardProvider;
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
    /// (spec `10-orchestrator.md` §9.39, §9.43; issue #332). `None` keeps
    /// the tool absent from the tool list — the additive disable path.
    pub board: Option<std::sync::Arc<dyn BoardProvider>>,
}

/// rmcp server exposing Orchestraitor built-in tools for one project scope.
#[derive(Clone)]
pub struct McpGateway {
    context: GatewayContext,
    fs: FileSystemTools,
    workflow: WorkflowTools,
}

impl McpGateway {
    /// Creates a gateway for a resolved project scope.
    #[must_use]
    pub fn new(context: GatewayContext) -> Self {
        let fs = FileSystemTools::new(context.scope.clone());
        Self {
            context,
            fs,
            workflow: WorkflowTools::new(),
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

#[tool_router]
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
    /// filter values are opaque data. Absent when no board provider is
    /// configured in the context.
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
                let result = run_board_query(provider.as_ref(), &mode, chain).await;
                self.structured(result)
            }
            None => Ok(CallToolResult::structured_error(serde_json::json!({
                "error": "board.query is not configured for this project scope",
                "code": "board_query_unconfigured"
            }))),
        }
    }
}

#[tool_handler(name = "orchestraitor-mcp", version = "0.0.0")]
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

/// Drives the typed board query against a provider with an audit store.
async fn run_board_query(
    provider: &dyn BoardProvider,
    mode: &BoardQueryMode,
    principals: Vec<String>,
) -> Result<BoardQueryResultKind, McpGatewayError> {
    let correlation_id = OperationId::new();
    let chain = DelegationChain {
        correlation_id,
        parent_op_id: None,
        principals,
    };
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
        });
        let _ = gateway;
        Ok(())
    }
}
