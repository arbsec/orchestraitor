//! MCP gateway, built-in tools, and canonical MCP configuration.
//!
//! The gateway routes project-scoped MCP calls and exposes Orchestraitor built-in tools. It is
//! not a sandbox or policy authority; Arbitraitor remains the exclusive security subsystem.

#![forbid(unsafe_code)]

mod config;
mod drift;
mod error;
mod fs;
mod fs_types;
mod gateway;
mod patch;
mod project;
mod workflow;

pub use config::{
    McpConfig, McpConfigLayer, McpServerConfig, McpServerLifetime, McpTransportConfig,
    ResolvedMcpServers, load_canonical_mcp_config, resolve_mcp_servers,
};
pub use drift::{
    CapabilityCrossCheck, CapabilitySnapshot, DriftFingerprint, FingerprintDigest, ServerIdentity,
    ToolSchemaIdentity, executable_sha256,
};
pub use error::{McpGatewayError, McpGatewayResult};
pub use fs::FileSystemTools;
pub use fs_types::{ApplyPatchRequest, DigestMismatch, FileDigest, ProjectPath};
pub use gateway::{GatewayContext, McpGateway};
pub use project::{ProjectId, ProjectScope};
pub use workflow::{WorkflowKind, WorkflowRequest, WorkflowTools};
