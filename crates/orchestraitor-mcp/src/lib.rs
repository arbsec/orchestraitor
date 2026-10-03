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

pub mod board_move;
pub mod board_query;
pub mod decision_record;

pub use board_move::{
    BoardMoveApplied, BoardMoveDelegationChain, BoardMoveError, BoardMoveOutcome, BoardMoveRefusal,
    BoardMoveRequest, BoardMoveResult, ClaimOutcome, InMemoryLeaseRegistry, ItemLease,
    LeaseBookkeepingFailure, LeaseRegistry, StatusClass, board_move,
};

pub use board_query::{
    BlockedCycle, BlockedGraph, BoardQueryField, BoardQueryFieldValue, BoardQueryFilter,
    BoardQueryItem, BoardQueryMode, BoardQueryResult, BoardQueryResultKind, DelegationChain,
    board_query,
};
pub use config::{
    McpConfig, McpConfigLayer, McpServerConfig, McpServerLifetime, McpTransportConfig,
    ResolvedMcpServers, load_canonical_mcp_config, resolve_mcp_servers,
};
pub use decision_record::{
    DecisionRecordAlternative, DecisionRecordError, DecisionRecordInput, DecisionRecordKind,
    DecisionRecordNoOpReason, DecisionRecordSelectedTask, DecisionRecordSkip,
    contains_secret_shaped, record_decision, validate_decision,
};
pub use drift::{
    CapabilityCrossCheck, CapabilitySnapshot, DriftFingerprint, FingerprintChange,
    FingerprintDigest, ServerIdentity, ToolSchemaIdentity, executable_sha256,
};
pub use error::{McpGatewayError, McpGatewayResult};
pub use fs::FileSystemTools;
pub use fs_types::{ApplyPatchRequest, DigestMismatch, FileDigest, ProjectPath};
pub use gateway::{GatewayContext, McpGateway};
pub use project::{ProjectId, ProjectScope};
pub use workflow::{WorkflowKind, WorkflowRequest, WorkflowTools};
