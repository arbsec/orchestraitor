//! The rmcp read-only tool surface over one loaded index snapshot.

use std::path::Path;

use orchestraitor_context::ContextIndex;
use orchestraitor_context::{ContextQuery, SymbolId};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{Implementation, ServerCapabilities, ServerInfo};
use rmcp::service::serve_server;
use rmcp::{ErrorData as McpError, ServerHandler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::CodegraphError;

/// Read-only codegraph MCP server over one index snapshot.
#[derive(Clone)]
pub struct CodegraphServer {
    index: std::sync::Arc<ContextIndex>,
    router: rmcp::handler::server::router::tool::ToolRouter<Self>,
}

/// Query parameters for `symbols`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct SymbolsRequest {
    /// Symbol name (exact match against the indexed display name).
    pub name: String,
    /// Optional repository-relative path prefix to scope the search.
    pub scope: Option<String>,
}

/// Query parameters for `symbol`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct SymbolRequest {
    /// The stable symbol id (from `symbols` results).
    pub symbol_id: String,
    /// Optional line budget for the body (default 200).
    pub line_budget: Option<u32>,
}

/// Query parameters for `references`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ReferencesRequest {
    /// The stable symbol id.
    pub symbol_id: String,
}

/// Query parameters for `calls`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct CallsRequest {
    /// The stable symbol id.
    pub symbol_id: String,
    /// `callers` (who calls this symbol) or `callees` (what this symbol calls).
    pub direction: String,
    /// Optional edge-count limit (default 50).
    pub limit: Option<usize>,
}

impl CodegraphServer {
    /// Loads the index from `project_root/.orchestraitor/codegraph.json`.
    ///
    /// # Errors
    /// Returns [`CodegraphError`] when the index is missing or corrupt.
    pub fn load(project_root: &Path) -> Result<Self, CodegraphError> {
        let index = crate::persist::load_index(project_root)?;
        let router = Self::static_tool_router();
        Ok(Self {
            index: std::sync::Arc::new(index),
            router,
        })
    }

    fn query(&self) -> ContextQuery<'_> {
        ContextQuery::new(&self.index)
    }
}

#[tool_router(router = static_tool_router, vis = "pub(crate)")]
impl CodegraphServer {
    /// Symbol name search.
    #[tool(description = "Find indexed symbols by exact name; optionally scoped to a path prefix.")]
    fn symbols(&self, Parameters(request): Parameters<SymbolsRequest>) -> Result<String, McpError> {
        let scope = request.scope.as_ref().map(std::path::PathBuf::from);
        let hits = self
            .query()
            .find_symbol(&request.name, None, scope.as_deref());
        serde_json::to_string(&hits)
            .map_err(|error| McpError::internal_error(error.to_string(), None))
    }

    /// Signature + bounded body for one symbol.
    #[tool(description = "Return the signature and bounded source body for one symbol id.")]
    fn symbol(&self, Parameters(request): Parameters<SymbolRequest>) -> Result<String, McpError> {
        let query = self.query();
        let symbol_id = SymbolId(request.symbol_id.clone());
        let item = query
            .symbol_signature(&symbol_id)
            .map_err(|error| McpError::internal_error(error.to_string(), None))?;
        let body = query
            .symbol_body(&symbol_id, request.line_budget)
            .map_err(|error| McpError::internal_error(error.to_string(), None))?;
        serde_json::to_string(&(item, body))
            .map_err(|error| McpError::internal_error(error.to_string(), None))
    }

    /// Textual references to a symbol.
    #[tool(description = "Return all textual references to the given symbol id.")]
    fn references(
        &self,
        Parameters(request): Parameters<ReferencesRequest>,
    ) -> Result<String, McpError> {
        let hits = self
            .query()
            .find_references(&SymbolId(request.symbol_id.clone()), None);
        serde_json::to_string(&hits)
            .map_err(|error| McpError::internal_error(error.to_string(), None))
    }

    /// Call-graph edges in one direction.
    #[tool(description = "Return callers or callees of the given symbol id.")]
    fn calls(&self, Parameters(request): Parameters<CallsRequest>) -> Result<String, McpError> {
        let symbol_id = SymbolId(request.symbol_id.clone());
        let limit = request.limit.or(Some(50));
        let edges = match request.direction.as_str() {
            "callers" => self.query().callers(&symbol_id, limit),
            "callees" => self.query().callees(&symbol_id, limit),
            other => {
                return Err(McpError::invalid_params(
                    format!("direction must be `callers` or `callees`, got `{other}`"),
                    None,
                ));
            }
        };
        serde_json::to_string(&edges)
            .map_err(|error| McpError::internal_error(error.to_string(), None))
    }
}

#[tool_handler(router = self.router)]
impl ServerHandler for CodegraphServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                "orchestraitor-codegraph",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "Read-only code intelligence over the indexed workspace: symbols, bodies, \
             references, and call-graph edges.",
            )
    }
}

/// Serves the codegraph server over stdio until the client disconnects.
///
/// # Errors
/// Returns [`CodegraphError::Transport`] when the rmcp stdio service fails.
pub fn serve_stdio(server: CodegraphServer) -> Result<(), CodegraphError> {
    let runtime = tokio::runtime::Runtime::new()
        .map_err(|error| CodegraphError::Transport(error.to_string()))?;
    runtime.block_on(async {
        let transport = rmcp::transport::stdio();
        let service = serve_server(server, transport)
            .await
            .map_err(|error| CodegraphError::Transport(error.to_string()))?;
        service
            .waiting()
            .await
            .map_err(|error| CodegraphError::Transport(error.to_string()))?;
        Ok(())
    })
}
