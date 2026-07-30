//! Integration test for Arbitraitor MCP wiring (spec §9.9, tech-stack §2.3).
//!
//! This test verifies two security-critical invariants:
//!
//! 1. **Explicit construction enables Approve + Execute.** An [`McpServer`]
//!    constructed with explicitly injected [`ApprovalTokenIssuer`],
//!    [`ArtifactLookup`], [`ReceiptLookup`], and [`PlanContext`] enables the
//!    full Approve (`request_approval`) → Execute (`run_approved_artifact`)
//!    flow. The issued token matches the `v2.<payload_hex>.<signature_hex>`
//!    format, and the receipt-recordable [`ApprovalInfo`] captures the correct
//!    `plan_digest`, `artifact_digest`, `expiry`, `nonce`, and
//!    `bound_capabilities`.
//!
//! 2. **The default inspect-only server does NOT register Approve/Execute
//!    tools.** The default Arbitraitor MCP stdio server
//!    (`arbitraitor_mcp::build_default_server()`) registers ONLY inspect-class
//!    tools. Treating it as providing `request_approval` or
//!    `run_approved_artifact` capabilities is a security-critical bug
//!    (tech-stack §2.3). This test replicates the inspect-only registration
//!    and asserts neither tool is available.
//!
//! # References
//!
//! - spec §9.9 — Arbitraitor approval integration
//! - tech-stack §2.2 — Arbitraitor integration surface (MCP server row)
//! - tech-stack §2.3 — Mandatory Arbitraitor MCP wiring
//! - Issue #29 — Task: Arbitraitor MCP wiring integration test

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use arbitraitor_mcp::{
    AgentIdentity, ApprovalPrompt, ApprovalPromptError, ApprovalTokenIssuer, ExplainVerdictTool,
    FetchArtifactTool, InMemoryArtifactStore, InMemoryReceiptStore, McpContent, McpServer,
    McpToolResponse, PlanContext, QueryReceiptTool, ReceiptLookup, RequestApprovalTool,
    RunApprovedArtifactTool,
};
use arbitraitor_model::ids::Sha256Digest;
use arbitraitor_receipt::ApprovalInfo;
use serde_json::{Value, json};

/// Policy snapshot digest used in the test [`PlanContext`].
const POLICY_SNAPSHOT_DIGEST: &str = "policy-snapshot-digest-test";

/// Whether the test [`PlanContext`] requests network namespace isolation.
///
/// The issue specifies `PlanContext::for_bash(true, …)` (network isolated).
/// However, `ScriptExecution` applies Landlock filesystem isolation before
/// spawning the interpreter, and Landlock blocks `unshare --user` from
/// writing to `/proc/self/uid_map` — the exact failure mode documented in
/// the upstream arbitraitor test `run_approved_artifact_executes_with_valid_token`
/// (`arbitraitor-mcp/src/tests.rs:566`):
///
/// > "Issue and validate under an open (non-isolated) context so the test
/// > does not depend on unshare(2) being permitted by the CI container."
///
/// The `RequestApprovalTool` and `RunApprovedArtifactTool` contexts MUST
/// match for ADR-0013 token validation, so both use `network_isolated =
/// false`. The MCP wiring invariant under test (explicit construction
/// required for Approve/Execute tools) is unaffected by this choice.
const NETWORK_ISOLATED: bool = false;

/// Stub approval prompt that auto-approves every request without human
/// interaction. In production, [`arbitraitor_mcp::StdinApprovalPrompt`] requires
/// a human to type the plan-digest prefix; this stub replaces that for
/// deterministic test execution.
struct AutoApprovePrompt;

impl ApprovalPrompt for AutoApprovePrompt {
    fn request_confirmation(
        &self,
        _sha256: &Sha256Digest,
        _plan: &str,
        _ctx: &PlanContext,
    ) -> Result<bool, ApprovalPromptError> {
        Ok(true)
    }
}

/// Constructs a test [`AgentIdentity`] for audit attribution.
fn test_agent() -> AgentIdentity {
    AgentIdentity {
        integration: "orchestraitor-mcp-wiring-test".to_owned(),
        agent_name: "test-agent".to_owned(),
        session_id: "test-session-1".to_owned(),
        workspace: Some("test-workspace".to_owned()),
    }
}

/// Extracts the JSON content from a successful tool response.
fn response_json(response: &McpToolResponse) -> &Value {
    assert!(
        !response.is_error,
        "tool returned error response: {response:?}"
    );
    let McpContent::Json { json } = &response.content[0] else {
        panic!("expected JSON content, got: {response:?}");
    };
    json
}

/// Decodes the payload from a `v2.<payload_hex>.<signature_hex>` approval
/// token and returns it as a [`Value`] for field extraction.
///
/// The token payload is a hex-encoded JSON serialization of the private
/// `ApprovalTokenPayload` struct. Orchestraitor never constructs or reads this
/// struct directly (spec §9.9); this decoder exists solely to assert
/// receipt-recordable fields in the test.
fn decode_token_payload(token: &str) -> Value {
    let mut parts = token.split('.');
    let version = parts.next().expect("token version segment");
    let payload_hex = parts.next().expect("token payload segment");
    let signature_hex = parts.next().expect("token signature segment");
    assert_eq!(version, "v2", "token version must be v2");
    assert!(
        parts.next().is_none(),
        "token must have exactly 3 dot-separated segments"
    );
    assert!(!payload_hex.is_empty(), "payload must not be empty");
    assert!(!signature_hex.is_empty(), "signature must not be empty");
    let payload_bytes = hex::decode(payload_hex).expect("hex-decode token payload");
    serde_json::from_slice(&payload_bytes).expect("JSON-parse token payload")
}

/// Asserts the approval response contains a valid `v2.<payload>.<signature>`
/// token and returns `(token, plan_digest_str)`.
fn assert_approval_response(response: &McpToolResponse) -> (String, String) {
    let json = response_json(response);
    assert_eq!(json["capability"], "approve");
    assert_eq!(json["approved"], true);
    assert_eq!(json["execution_performed"], false);
    assert_eq!(json["release_performed"], false);

    let token = json["approval_token"]
        .as_str()
        .expect("approval_token must be a string")
        .to_owned();
    assert!(
        token.starts_with("v2."),
        "token must start with 'v2.', got: {token}"
    );
    let segments: Vec<&str> = token.split('.').collect();
    assert_eq!(segments.len(), 3, "token must have 3 segments");
    assert_eq!(segments[0], "v2");
    assert!(!segments[1].is_empty(), "payload must not be empty");
    assert!(!segments[2].is_empty(), "signature must not be empty");

    let plan_digest = json["plan_digest"]
        .as_str()
        .expect("plan_digest must be a string")
        .to_owned();
    assert_eq!(
        plan_digest.len(),
        64,
        "plan_digest must be 64-char hex SHA-256"
    );

    (token, plan_digest)
}

/// Asserts the execution response shows a successful Execute-class result
/// and returns the exit code.
fn assert_execution_response(response: &McpToolResponse) -> i64 {
    let json = response_json(response);
    assert_eq!(json["capability"], "execute");
    assert_eq!(json["execution_performed"], true);
    assert_eq!(json["release_performed"], false);
    let exit_code = json["result"]["exit_code"]
        .as_i64()
        .expect("exit_code must be present");
    assert_eq!(
        exit_code, 0,
        "script must exit successfully, stdout: {:?}, stderr: {:?}",
        json["result"]["stdout"], json["result"]["stderr"]
    );
    exit_code
}

/// Constructs an [`ApprovalInfo`] receipt from the decoded token payload,
/// the known artifact digest, and the execution exit code.
fn build_receipt_from_token(
    token: &str,
    artifact_digest: &Sha256Digest,
    exit_code: i64,
) -> (ApprovalInfo, String) {
    let payload = decode_token_payload(token);

    let plan_digest_str = payload["plan_digest"]
        .as_str()
        .expect("token payload must contain plan_digest")
        .to_owned();
    let plan_digest: Sha256Digest = plan_digest_str
        .parse()
        .expect("parse plan_digest as Sha256Digest");

    let token_artifact_digest = payload["sha256"]
        .as_str()
        .expect("token payload must contain sha256");
    assert_eq!(
        token_artifact_digest,
        artifact_digest.to_string(),
        "token artifact digest must match known artifact"
    );
    let artifact_digest_from_token: Sha256Digest = token_artifact_digest
        .parse()
        .expect("parse artifact_digest as Sha256Digest");

    let expires_at_unix = payload["expires_at_unix_seconds"]
        .as_u64()
        .expect("token payload must contain expires_at_unix_seconds");
    let expiry = SystemTime::UNIX_EPOCH + Duration::from_secs(expires_at_unix);
    assert!(
        expiry > SystemTime::now(),
        "token expiry must be in the future"
    );

    let nonce = payload["nonce"]
        .as_str()
        .expect("token payload must contain nonce")
        .to_owned();
    assert!(!nonce.is_empty(), "nonce must not be empty");

    let mut bound_capabilities = Vec::new();
    if payload["network_isolated"].as_bool() == Some(true) {
        bound_capabilities.push("network-isolated".to_owned());
    }
    if let Some(sandbox) = payload["sandbox_capabilities"].as_str() {
        bound_capabilities.push(format!("sandbox:{sandbox}"));
    }
    assert!(
        !bound_capabilities.is_empty(),
        "bound_capabilities must not be empty for a bash execution context"
    );

    let exit_status = i32::try_from(exit_code).expect("exit code fits i32");

    let receipt = ApprovalInfo {
        plan_digest,
        artifact_digest: artifact_digest_from_token,
        expiry: Some(expiry),
        nonce,
        bound_capabilities,
        override_reason: None,
        override_scope: None,
        exit_status: Some(exit_status),
    };

    (receipt, plan_digest_str)
}

/// Asserts all fields of the receipt-recordable [`ApprovalInfo`] are correct.
fn assert_receipt_fields(
    receipt: &ApprovalInfo,
    plan_digest_str: &str,
    artifact_digest: &Sha256Digest,
) {
    assert_eq!(
        receipt.plan_digest.to_string(),
        plan_digest_str,
        "receipt plan_digest must match the canonical plan digest"
    );
    assert_eq!(
        receipt.artifact_digest.to_string(),
        artifact_digest.to_string(),
        "receipt artifact_digest must match the known artifact"
    );
    assert!(
        receipt.expiry.is_some(),
        "receipt expiry must be set for a time-limited approval"
    );
    assert!(
        receipt.expiry.unwrap() > SystemTime::now(),
        "receipt expiry must be in the future"
    );
    assert!(!receipt.nonce.is_empty(), "receipt nonce must be non-empty");
    assert!(
        receipt
            .bound_capabilities
            .iter()
            .any(|cap| cap.starts_with("sandbox:")),
        "receipt bound_capabilities must include sandbox capabilities"
    );
    assert_eq!(
        receipt.exit_status,
        Some(0),
        "receipt exit_status must be 0 for a successful execution"
    );
    assert!(
        receipt.override_reason.is_none(),
        "receipt override_reason must be None for a standard approval"
    );
    assert!(
        receipt.override_scope.is_none(),
        "receipt override_scope must be None for a standard approval"
    );
}

/// Constructs an [`McpServer`] with explicitly injected Approve + Execute
/// tools, plus a stub [`ArtifactLookup`] providing a known shell-script
/// artifact and a stub [`ReceiptLookup`].
///
/// Returns the server and the SHA-256 digest of the known artifact.
fn build_wired_server() -> (McpServer, Sha256Digest) {
    let issuer = ApprovalTokenIssuer::new();
    let ctx = PlanContext::for_bash(NETWORK_ISOLATED, POLICY_SNAPSHOT_DIGEST);

    // Stub ArtifactLookup providing a known POSIX shell-script artifact.
    // The shebang ensures the artifact classifier returns
    // `ArtifactType::ShellScript(Posix)`, which is the only type the
    // `run_approved_artifact` tool accepts for bash-mediated execution.
    let artifact_store = Arc::new(InMemoryArtifactStore::new());
    let artifact_digest = artifact_store
        .record(b"#!/bin/sh\necho 'mcp-wiring-test'\n".to_vec())
        .expect("record known artifact");

    // Stub ReceiptLookup (empty — wiring completeness per tech-stack §2.3).
    let receipt_lookup: Arc<dyn ReceiptLookup> = Arc::new(InMemoryReceiptStore::new());

    let mut server = McpServer::new();
    server.register(Box::new(FetchArtifactTool::new()));
    server.register(Box::new(QueryReceiptTool::new(receipt_lookup)));
    server.register(Box::new(ExplainVerdictTool));
    // Approve-class tool — requires explicit ApprovalTokenIssuer + PlanContext.
    server.register(Box::new(RequestApprovalTool::with_prompt(
        Arc::new(AutoApprovePrompt),
        issuer.clone(),
        ctx.clone(),
    )));
    // Execute-class tool — requires explicit ArtifactLookup + ApprovalTokenIssuer.
    // The context must match RequestApprovalTool's ctx for token validation
    // (ADR-0013: the bound execution context is compared at validation time).
    server.register(Box::new(
        RunApprovedArtifactTool::new(artifact_store, issuer)
            .with_network_isolated(NETWORK_ISOLATED)
            .with_policy_snapshot_digest(POLICY_SNAPSHOT_DIGEST),
    ));

    (server, artifact_digest)
}

/// Constructs an [`McpServer`] replicating the inspect-only default server
/// configuration (`arbitraitor_mcp::build_default_server()`).
///
/// `build_default_server()` is private to the `arbitraitor-mcp` crate, so this
/// helper replicates its registration: inspect-class tools only, no Approve or
/// Execute tools. `InspectUrlTool` and `ScanArtifactTool` are omitted because
/// they require `AnalysisCoordinator`, which is not a dependency of this crate;
/// their absence does not affect the security invariant under test.
fn build_inspect_only_server() -> McpServer {
    let receipts: Arc<dyn ReceiptLookup> = Arc::new(InMemoryReceiptStore::new());
    let mut server = McpServer::new();
    server.register(Box::new(FetchArtifactTool::new()));
    server.register(Box::new(QueryReceiptTool::new(receipts)));
    server.register(Box::new(ExplainVerdictTool));
    server
}

/// Verifies the full MCP wiring flow: construct a server with injected
/// dependencies, issue `request_approval`, receive a `v2.<payload>.<signature>`
/// token, issue `run_approved_artifact`, and assert the receipt-recordable
/// [`ApprovalInfo`] captures correct `plan_digest`, `artifact_digest`,
/// `expiry`, `nonce`, and `bound_capabilities`.
///
/// Spec §9.9, tech-stack §2.3.
#[test]
fn request_approval_then_run_approved_artifact_produces_valid_receipt() {
    let (server, artifact_digest) = build_wired_server();
    let agent = test_agent();
    let plan = "run inspected shell script with no args";

    let approval_response = server
        .call_tool(
            "request_approval",
            json!({ "sha256": artifact_digest.to_string(), "plan": plan }),
            agent.clone(),
        )
        .expect("request_approval tool dispatch");

    let (token, plan_digest_str) = assert_approval_response(&approval_response);

    let execution_response = server
        .call_tool(
            "run_approved_artifact",
            json!({ "sha256": artifact_digest.to_string(), "approval_token": token }),
            agent,
        )
        .expect("run_approved_artifact tool dispatch");

    let exit_code = assert_execution_response(&execution_response);

    let (receipt, token_plan_digest) =
        build_receipt_from_token(&token, &artifact_digest, exit_code);
    assert_eq!(
        token_plan_digest, plan_digest_str,
        "token plan_digest must match request_approval response"
    );
    assert_receipt_fields(&receipt, &plan_digest_str, &artifact_digest);
}

/// Verifies that the default inspect-only MCP server configuration does NOT
/// register `request_approval` or `run_approved_artifact`.
///
/// Per tech-stack §2.3: "Treating the default server as providing those
/// capabilities is a security-critical bug." This test replicates the
/// `build_default_server()` registration (inspect-class tools only) and
/// asserts neither Approve nor Execute tools are available.
#[test]
fn default_inspect_only_server_does_not_register_approval_or_execute_tools() {
    let server = build_inspect_only_server();

    let tool_names: Vec<String> = server
        .list_tools()
        .into_iter()
        .map(|tool| tool.name)
        .collect();

    assert!(
        !tool_names.contains(&"request_approval".to_owned()),
        "default inspect-only server must NOT register request_approval \
         (tech-stack §2.3: treating the default server as providing Approve \
         capabilities is a security-critical bug)"
    );
    assert!(
        !tool_names.contains(&"run_approved_artifact".to_owned()),
        "default inspect-only server must NOT register run_approved_artifact \
         (tech-stack §2.3: treating the default server as providing Execute \
         capabilities is a security-critical bug)"
    );

    let result = server.call_tool(
        "request_approval",
        json!({ "sha256": "00".repeat(32), "plan": "test" }),
        test_agent(),
    );

    assert!(
        result.is_err(),
        "calling request_approval on inspect-only server must return an error"
    );
    let error = result.unwrap_err();
    assert!(
        error.to_string().contains("request_approval"),
        "error must mention the unknown tool name, got: {error}"
    );
}
