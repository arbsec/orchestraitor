//! Throwaway evidence harness (not committed): drives the REAL gateway
//! `decision.record` tool end to end over rmcp's in-memory transport.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stdout,
    clippy::useless_conversion,
    clippy::redundant_closure,
    clippy::too_many_lines
)]
//! `decision.record` tool end to end over rmcp's in-memory transport —
//! JSON-RPC request through `serve_directly`, `ServerHandler` dispatch,
//! structured result read back — for the happy persist+replay scenario and
//! the typed-refusal scenario (issue #334 QA evidence).

use orchestraitor_arbitraitor_client::ArbitraitorClient;
use orchestraitor_mcp::{GatewayContext, McpGateway, ProjectScope, ResolvedMcpServers};
use rmcp::RoleServer;
use rmcp::service::RunningService;
use rmcp::service::serve_directly;

/// One live gateway session over an in-memory duplex transport: the SAME
/// store instance backs every `decision.record` call on this session.
struct Session {
    running: RunningService<RoleServer, McpGateway>,
    client_r: tokio::io::ReadHalf<tokio::io::DuplexStream>,
    client_w: tokio::io::WriteHalf<tokio::io::DuplexStream>,
}

impl Session {
    fn open() -> Self {
        let (client_io, server_io) = tokio::io::duplex(8192);
        let (server_r, server_w) = tokio::io::split(server_io);
        let (client_r, client_w) = tokio::io::split(client_io);
        let temp = tempfile::tempdir().unwrap();
        let scope = ProjectScope::from_root(temp.path()).unwrap();
        std::mem::forget(temp); // evidence run: scope lives for process life
        let gateway = McpGateway::new(GatewayContext {
            scope,
            servers: ResolvedMcpServers::default(),
            arbitraitor: ArbitraitorClient::default(),
            board: None,
            board_audit_store: None,
            // Session-scoped decision store: every decision.record call on
            // this session appends into the SAME store, so append-only row
            // ids are observable across invocations.
            decision_store: Some(std::sync::Arc::new(std::sync::Mutex::new(
                orchestraitor_campaign::CampaignDecisionStore::open_in_memory().unwrap(),
            ))),
        });
        let running = serve_directly(gateway, (server_r, server_w), None);
        running
            .peer()
            .set_peer_info(rmcp::model::ClientInfo::default().into());
        Self {
            running,
            client_r,
            client_w,
        }
    }

    /// One `tools/call` for `decision.record` through the REAL dispatch
    /// path; returns the structured tool result.
    async fn call(&mut self, arguments: serde_json::Value) -> serde_json::Value {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {"name": "decision.record", "arguments": arguments}
        });
        let mut line = serde_json::to_string(&request).unwrap();
        line.push('\n');
        self.client_w.write_all(line.as_bytes()).await.unwrap();
        self.client_w.flush().await.unwrap();

        let mut reader = tokio::io::BufReader::new(&mut self.client_r);
        let mut response = String::new();
        reader.read_line(&mut response).await.unwrap();
        println!("[wire] {}", response.trim());
        let parsed: serde_json::Value = serde_json::from_str(response.trim()).unwrap();
        parsed["result"].clone()
    }

    async fn close(self) {
        let mut running = self.running;
        let _ = running.close().await;
    }
}

/// Normalizes one tool result to the tool's structured payload: the
/// structuredContent object when present, else the object-valued sibling
/// the handler emitted under a prefixed key, else the raw result.
fn normalize_tool_result(result: serde_json::Value) -> serde_json::Value {
    result
        .get("structuredContent")
        .cloned()
        .filter(serde_json::Value::is_object)
        .or_else(|| {
            result.as_object().and_then(|object| {
                object
                    .iter()
                    .find(|(key, value)| key != &"content" && value.is_object())
                    .map(|(_, value)| value.clone())
            })
        })
        .unwrap_or(result)
}

#[tokio::main]
async fn main() {
    println!("=== decision.record — happy persist + replay (real gateway dispatch path) ===\n");

    let happy = serde_json::json!({
        "kind": "selected",
        "selected": {
            "repo": "arbsec/orchestraitor",
            "number": 334,
            "title": "Task: tools-decision-record — decision.record (append-only, replayable)",
            "url": "https://github.com/arbsec/orchestraitor/issues/334",
            "item_node_id": "PVTI_1",
            "task_id": "board-arbsec_orchestraitor-334"
        },
        "role": "implement",
        "provider": "neuralwatt",
        "model": "glm-5.2",
        "precedence_path": "bootstrap-default",
        "worker_args": ["worker", "run", "--task", "board-arbsec_orchestraitor-334", "--json"],
        "rationale": "first eligible P0 item in P0-first ready order (spec 10-orchestrator.md §9.35)",
        "alternatives": [{
            "repo": "arbsec/arbitraitor",
            "number": 42,
            "title": "alternative candidate",
            "open_blockers": 0,
            "target": "MVP",
            "status": "Ready"
        }],
        "delegation_chain": ["user:alice", "session:sess_7e3f"]
    });

    println!("-- tool call: decision.record (spawn decision, all §9.35 fields)");
    println!("{happy:#}");

    let mut session = Session::open();
    let raw_stored = session.call(happy.clone()).await;
    println!(
        "\n-- raw JSON-RPC result (first call) — keys: {:?}",
        raw_stored
            .as_object()
            .map(|o| o.keys().cloned().collect::<Vec<_>>())
    );
    let stored = normalize_tool_result(raw_stored);
    println!("\n-- tool result (stored record, read back from the store)");
    println!("{stored:#}");

    println!(
        "\n-- replay: the SAME record appended again on the SAME session (append-only semantics)"
    );
    let raw_again = session.call(happy).await;
    let again = normalize_tool_result(raw_again);
    let stored_content = &stored;
    let again_content = &again;
    println!(
        "first record_id = {}, replay record_id = {}",
        stored_content["id"], again_content["id"]
    );
    let first_id = stored_content["id"].as_i64().expect("stored row id");
    let again_id = again_content["id"].as_i64().expect("replay row id");
    assert!(
        again_id > first_id,
        "append-only: re-append creates a NEW row"
    );
    assert_eq!(
        stored_content["decision"], again_content["decision"],
        "replay of the same decision persists an identical payload"
    );
    println!(
        "payloads byte-identical: {}",
        stored_content["decision"] == again_content["decision"]
    );
    session.close().await;

    println!("\nOK: happy persist + replay verified through the real tool dispatch path");

    println!("\n=== decision.record — typed refusal (missing required fields) ===\n");
    let malformed = serde_json::json!({
        "kind": "selected",
        "role": "implement",
        "provider": "neuralwatt",
        "model": "glm-5.2",
        "precedence_path": "bootstrap-default",
        "rationale": "no selected task"
    });
    println!("-- tool call: decision.record (a `selected` decision WITHOUT the selected task)");
    println!("{malformed:#}");
    let mut refusal_session = Session::open();
    let refused = normalize_tool_result(refusal_session.call(malformed).await);
    println!("\n-- tool result (typed refusal)");
    println!("{refused:#}");
    let error_text = refused["error"].as_str().unwrap_or_default();
    assert!(
        error_text.contains("must carry the selected task"),
        "refusal must be typed: {refused}"
    );
    assert_eq!(refused["code"], "decision_record_failed");

    println!("\n-- tool call: decision.record (rationale carries a secret:// URI)");
    let secret = serde_json::json!({
        "kind": "no-op",
        "no_op_reason": "empty-queue",
        "role": "implement",
        "provider": "neuralwatt",
        "model": "glm-5.2",
        "precedence_path": "bootstrap-default",
        "rationale": "queue empty; use token secret://env/GH_TOKEN for board auth"
    });
    let refused_secret = normalize_tool_result(refusal_session.call(secret).await);
    println!("\n-- tool result (secret-shape refusal)");
    println!("{refused_secret:#}");
    assert!(
        refused_secret["error"]
            .as_str()
            .unwrap_or_default()
            .contains("secret-shaped"),
        "secret refusal must be typed: {refused_secret}"
    );
    assert!(
        !serde_json::to_string(&refused_secret)
            .unwrap()
            .contains("GH_TOKEN"),
        "the secret value itself must never appear in the error"
    );
    refusal_session.close().await;

    println!("\nOK: typed refusals verified; nothing was appended, nothing recorded");
}
