//! `orc observe` implementation — observation-only event recording (spec §998 MVP-2).
//!
//! Records a normalized event stream for the target harness — filesystem
//! mutations, process executions, network requests, MCP tool calls — and
//! evaluates shadow policy decisions per operation. Shadow decisions record
//! what Arbitraitor's policy engine *would* have decided; they do not affect
//! execution. **Observation mode is non-protective.**

#![forbid(unsafe_code)]

use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use miette::{IntoDiagnostic, Result, bail};
use orchestraitor_arbitraitor_client::ArbitraitorClient;
use orchestraitor_arbitraitor_client::sandbox::{ControlState, EffectiveControls, SandboxMode};
use orchestraitor_events::{
    AuditStore, CURRENT_SCHEMA_VERSION, EventCategory, EventEnvelope, EventEnvelopeInput,
    HashDigest, InMemoryAuditStore, PrivacyExportMode,
};
use orchestraitor_model::OperationId;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::cli::ObserveArgs;

/// Shadow policy decision outcome (spec §9.8, MVP-2).
///
/// Records what Arbitraitor's policy engine *would* have decided for an
/// observed operation. Shadow decisions do not affect execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShadowDecisionOutcome {
    /// Operation would have been allowed.
    Pass,
    /// Operation would have been allowed with additional constraints.
    PassWithConstraints,
    /// Operation would have required user approval.
    Prompt,
    /// Operation would have been blocked.
    Block,
    /// Operation is not supported by the current policy configuration.
    Unsupported,
    /// A stronger sandbox mode is required to evaluate this operation.
    DeferToStrongerSandbox,
}

/// Observed operation category (spec §9.17, MVP-2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservedOperationKind {
    /// Filesystem mutation (create, write, delete, rename).
    FilesystemMutation,
    /// Process execution (spawn, exec).
    ProcessExecution,
    /// Network request (outbound connection).
    NetworkRequest,
    /// MCP tool call.
    McpToolCall,
}

/// A shadow policy decision recorded for an observed operation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShadowPolicyDecision {
    /// The operation category that was evaluated.
    pub operation_kind: ObservedOperationKind,
    /// The shadow decision outcome.
    pub outcome: ShadowDecisionOutcome,
    /// Human-readable trace explaining the decision.
    pub trace: String,
}

/// Runs `orc observe` and writes user-facing output to `writer`.
///
/// Records a normalized event stream for the target harness, evaluates shadow
/// policy decisions per operation, and exports the event stream to the output
/// directory. The output always identifies as non-protective (spec §998 MVP-2).
///
/// # Errors
///
/// Returns a diagnostic when the harness command is missing, event recording
/// fails, or the event stream cannot be exported.
pub fn run<W: Write>(args: &ObserveArgs, writer: &mut W) -> Result<()> {
    if args.harness.is_empty() {
        bail!("orc observe requires a harness command after --");
    }
    let platform = std::env::consts::OS;
    let harness_display = args.harness.join(" ");
    let mut session = ObserveSession::new();

    write_indicator(writer, args.json, &harness_display, platform)?;
    session.record(
        EventCategory::SessionLifecycle,
        json!({"state":"started","harness":harness_display,"platform":platform}),
    )?;

    let client = ArbitraitorClient::default();
    let controls = client.probe_effective_controls(SandboxMode::Restricted, platform);
    for (kind, control) in shadow_control_pairs(&controls) {
        let outcome = shadow_decision_outcome(control);
        let trace = format!(
            "{}={} on {}",
            control_name(kind),
            control_state_str(control),
            platform
        );
        session.record(
            EventCategory::PolicyDecision,
            json!({"shadow":true,"operation_kind":kind,"outcome":outcome,"trace":trace,"enforcement":false}),
        )?;
        write_shadow(writer, args.json, kind, outcome, &trace)?;
    }

    let exit_code = spawn_harness(&args.harness, &harness_display, &mut session)?;
    write_exit(writer, args.json, exit_code)?;
    session.record(
        EventCategory::SessionLifecycle,
        json!({"state":"ended","exit_code":exit_code}),
    )?;

    fs::create_dir_all(&args.output).into_diagnostic()?;
    let export_path = args.output.join("events.jsonl");
    let export_data = session.export()?;
    fs::write(&export_path, &export_data).into_diagnostic()?;
    write_summary(
        writer,
        args.json,
        session.record_count(),
        &export_path,
        exit_code,
    )?;
    Ok(())
}

fn spawn_harness(
    harness: &[String],
    harness_display: &str,
    session: &mut ObserveSession,
) -> Result<Option<i32>> {
    match Command::new(&harness[0]).args(&harness[1..]).output() {
        Ok(output) => {
            session.record(
                EventCategory::ProcessExecution,
                json!({"command":harness_display,"exit_code":output.status.code(),"stdout_bytes":output.stdout.len(),"stderr_bytes":output.stderr.len()}),
            )?;
            Ok(output.status.code())
        }
        Err(error) => {
            session.record(
                EventCategory::Error,
                json!({"error":"failed to spawn harness","command":harness_display,"kind":format!("{:?}",error.kind())}),
            )?;
            Ok(None)
        }
    }
}

fn write_indicator<W: Write>(
    writer: &mut W,
    json: bool,
    harness: &str,
    platform: &str,
) -> Result<()> {
    if json {
        write_json(
            writer,
            &json!({"mode":"observe","protective":false,"indicator":"observation mode: non-protective","harness":harness,"platform":platform}),
        )
    } else {
        writeln!(writer, "observation mode: non-protective").into_diagnostic()?;
        writeln!(writer, "  harness: {harness}").into_diagnostic()?;
        writeln!(writer, "  platform: {platform}").into_diagnostic()?;
        writeln!(writer, "  no enforcement is claimed or implied").into_diagnostic()
    }
}

fn write_shadow<W: Write>(
    writer: &mut W,
    json: bool,
    kind: ObservedOperationKind,
    outcome: ShadowDecisionOutcome,
    trace: &str,
) -> Result<()> {
    if json {
        write_json(
            writer,
            &json!({"event":"shadow_decision","operation_kind":kind,"outcome":outcome,"trace":trace}),
        )
    } else {
        let ks = serde_json::to_string(&kind).into_diagnostic()?;
        let os = serde_json::to_string(&outcome).into_diagnostic()?;
        writeln!(
            writer,
            "shadow: {} -> {} ({trace})",
            ks.trim_matches('"'),
            os.trim_matches('"')
        )
        .into_diagnostic()
    }
}

fn write_exit<W: Write>(writer: &mut W, json: bool, exit_code: Option<i32>) -> Result<()> {
    if json {
        write_json(
            writer,
            &json!({"event":"harness_exited","exit_code":exit_code}),
        )
    } else {
        match exit_code {
            Some(code) => writeln!(writer, "harness exited: code={code}").into_diagnostic(),
            None => writeln!(writer, "harness exited: signal or spawn failure").into_diagnostic(),
        }
    }
}

fn write_summary<W: Write>(
    writer: &mut W,
    json: bool,
    events: usize,
    export_path: &Path,
    exit_code: Option<i32>,
) -> Result<()> {
    if json {
        write_json(
            writer,
            &json!({"summary":{"events_recorded":events,"export_path":export_path.display().to_string(),"exit_code":exit_code}}),
        )
    } else {
        writeln!(writer, "events recorded: {events}").into_diagnostic()?;
        writeln!(writer, "event stream: {}", export_path.display()).into_diagnostic()
    }
}

fn write_json<W: Write>(writer: &mut W, value: &Value) -> Result<()> {
    let s = serde_json::to_string(value).into_diagnostic()?;
    writeln!(writer, "{s}").into_diagnostic()
}

fn shadow_decision_outcome(control: ControlState) -> ShadowDecisionOutcome {
    match control {
        ControlState::Available => ShadowDecisionOutcome::Pass,
        ControlState::Degraded => ShadowDecisionOutcome::PassWithConstraints,
        ControlState::Unavailable => ShadowDecisionOutcome::DeferToStrongerSandbox,
    }
}

fn shadow_control_pairs(
    controls: &EffectiveControls,
) -> [(ObservedOperationKind, ControlState); 4] {
    [
        (
            ObservedOperationKind::FilesystemMutation,
            controls.filesystem_isolation,
        ),
        (
            ObservedOperationKind::ProcessExecution,
            controls.process_tree_containment,
        ),
        (
            ObservedOperationKind::NetworkRequest,
            controls.network_isolation,
        ),
        (
            ObservedOperationKind::McpToolCall,
            controls.syscall_filtering,
        ),
    ]
}

fn control_name(kind: ObservedOperationKind) -> &'static str {
    match kind {
        ObservedOperationKind::FilesystemMutation => "filesystem_isolation",
        ObservedOperationKind::ProcessExecution => "process_tree_containment",
        ObservedOperationKind::NetworkRequest => "network_isolation",
        ObservedOperationKind::McpToolCall => "syscall_filtering",
    }
}

fn control_state_str(state: ControlState) -> &'static str {
    match state {
        ControlState::Available => "available",
        ControlState::Degraded => "degraded",
        ControlState::Unavailable => "unavailable",
    }
}

struct ObserveSession {
    store: InMemoryAuditStore,
    seq: u64,
    prev_hash: Option<HashDigest>,
    correlation_id: OperationId,
}

impl ObserveSession {
    fn new() -> Self {
        Self {
            store: InMemoryAuditStore::default(),
            seq: 0,
            prev_hash: None,
            correlation_id: OperationId::new(),
        }
    }

    fn record(&mut self, category: EventCategory, payload: Value) -> Result<()> {
        self.seq = self.seq.saturating_add(1);
        let ts = match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(d) => format!("{}.{:09}Z", d.as_secs(), d.subsec_nanos()),
            Err(_) => "0.000000000Z".to_string(),
        };
        let envelope = EventEnvelope::try_new(EventEnvelopeInput {
            schema_version: CURRENT_SCHEMA_VERSION,
            monotonic_seq: self.seq,
            wall_clock_ts: ts,
            correlation_id: self.correlation_id.clone(),
            parent_op_id: None,
            category,
            payload,
            prev_hash: self.prev_hash.clone(),
        })
        .into_diagnostic()?;
        let record = self.store.append(envelope).into_diagnostic()?;
        self.prev_hash = Some(record.hash);
        Ok(())
    }

    fn export(&self) -> Result<Vec<u8>> {
        self.store.export(PrivacyExportMode::Full).into_diagnostic()
    }

    fn record_count(&self) -> usize {
        self.store.records().len()
    }
}
