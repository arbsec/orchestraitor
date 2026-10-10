//! Built-in workflow tools for configured verification tasks.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::{McpGatewayError, McpGatewayResult};

/// Built-in workflow tool category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowKind {
    /// Formatting workflow.
    Format,
    /// Lint workflow.
    Lint,
    /// Type or compile check workflow.
    Check,
    /// Test workflow.
    Test,
    /// Named task workflow.
    Task,
}

/// Workflow run request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkflowRequest {
    /// Optional task name for `task.run`.
    pub name: Option<String>,
}

/// Workflow run result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkflowResult {
    /// Workflow category.
    pub kind: WorkflowKind,
    /// Status string.
    pub status: String,
    /// Human-readable explanation.
    pub message: String,
}

/// Built-in workflow tools.
#[derive(Debug, Clone, Default)]
pub struct WorkflowTools;

impl WorkflowTools {
    /// Creates workflow tools.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// Prepares a workflow run without launching an uninspected process.
    ///
    /// # Errors
    /// Returns an inspection-required error until a caller supplies Arbitraitor-approved execution.
    pub fn run(
        &self,
        kind: WorkflowKind,
        request: &WorkflowRequest,
    ) -> McpGatewayResult<WorkflowResult> {
        let _ = self;
        let server_id = match (&kind, &request.name) {
            (WorkflowKind::Task, Some(name)) => format!("task:{name}"),
            (WorkflowKind::Task, None) => "task".to_string(),
            (WorkflowKind::Format, _) => "format".to_string(),
            (WorkflowKind::Lint, _) => "lint".to_string(),
            (WorkflowKind::Check, _) => "check".to_string(),
            (WorkflowKind::Test, _) => "test".to_string(),
        };
        Err(McpGatewayError::ArbitraitorInspectionRequired { server_id })
    }
}
