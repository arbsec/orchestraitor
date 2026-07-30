//! `orc evidence export` implementation (spec MVP-8, §9.17.1).
//!
//! Produces a privacy-preserving archive of session evidence. Secrets,
//! prompts, completions, tool arguments, and MCP payloads are always
//! redacted (spec §9.17.1). The export uses the tamper-evident hash-chained
//! event store from `orchestraitor-events`.

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use miette::IntoDiagnostic;
use orchestraitor_events::{AuditStore, EventError, InMemoryAuditStore, PrivacyExportMode};

use crate::cli::{ConfigPaths, EvidenceExportArgs};
use crate::exit_code::{ExitCode, OrcError, OrcResult};

/// Runs `orc evidence export`.
///
/// # Errors
///
/// Returns [`ExitCode::ConfigError`] when the event store cannot be read,
/// [`ExitCode::InfrastructureFailure`] for I/O errors.
pub fn run<W: Write>(
    paths: &ConfigPaths,
    args: &EvidenceExportArgs,
    writer: &mut W,
) -> OrcResult<ExitCode> {
    let store = load_event_store(paths, args.session.as_deref())?;
    let mode = if args.full {
        PrivacyExportMode::Full
    } else {
        PrivacyExportMode::Redacted
    };
    let bytes = store
        .export(mode)
        .map_err(|e| event_error_to_orc(&e, "event export failed"))?;

    let record_count = count_records(&bytes);

    match &args.output {
        Some(path) => {
            write_to_file(path, &bytes)?;
        }
        None => {
            writer
                .write_all(&bytes)
                .into_diagnostic()
                .map_err(OrcError::infrastructure)?;
        }
    }

    if !args.quiet {
        let mode_label = if args.full { "full" } else { "redacted" };
        if let Some(ref path) = args.output {
            writeln!(
                writer,
                "exported {record_count} events ({mode_label}) to {}",
                path.display()
            )
            .into_diagnostic()
            .map_err(infra_error)?;
        } else if args.json {
            let summary = ExportSummary {
                record_count,
                mode: mode_label.to_string(),
                output: None,
            };
            serde_json::to_writer_pretty(&mut *writer, &summary)
                .into_diagnostic()
                .map_err(infra_error)?;
            writeln!(writer).into_diagnostic().map_err(infra_error)?;
        } else {
            writeln!(writer, "exported {record_count} events ({mode_label})")
                .into_diagnostic()
                .map_err(infra_error)?;
        }
    }

    Ok(ExitCode::Success)
}

fn infra_error(e: miette::Report) -> OrcError {
    OrcError::infrastructure(e)
}

fn load_event_store(paths: &ConfigPaths, session: Option<&str>) -> OrcResult<InMemoryAuditStore> {
    let event_path = event_store_path(paths, session);
    let bytes = match fs::read(&event_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(InMemoryAuditStore::default());
        }
        Err(error) => {
            return Err(OrcError::infrastructure(miette::Report::msg(format!(
                "failed to read event store: {}: {error}",
                event_path.display()
            ))));
        }
    };
    let mut store = InMemoryAuditStore::default();
    store
        .r#import(&bytes)
        .map_err(|e| event_error_to_orc(&e, "event store hash-chain validation failed"))?;
    Ok(store)
}

fn event_error_to_orc(error: &EventError, context: &str) -> OrcError {
    OrcError::infrastructure(miette::Report::msg(format!("{context}: {error}")))
}

fn event_store_path(paths: &ConfigPaths, session: Option<&str>) -> PathBuf {
    let dir = paths.config_dir.join("events");
    match session {
        Some(id) => dir.join(format!("{id}.jsonl")),
        None => dir.join("current.jsonl"),
    }
}

fn write_to_file(path: &std::path::Path, bytes: &[u8]) -> OrcResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .into_diagnostic()
            .map_err(OrcError::infrastructure)?;
    }
    fs::write(path, bytes)
        .into_diagnostic()
        .map_err(OrcError::infrastructure)
}

#[allow(
    clippy::naive_bytecount,
    reason = "record count is not performance-critical"
)]
fn count_records(bytes: &[u8]) -> usize {
    bytes.iter().filter(|&&b| b == b'\n').count()
}

/// JSON summary of an export operation.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ExportSummary {
    /// Number of event records exported.
    pub record_count: usize,
    /// Privacy mode used for the export.
    pub mode: String,
    /// Output file path, if written to a file.
    pub output: Option<String>,
}

#[allow(clippy::unwrap_used, clippy::expect_used)]
#[cfg(test)]
mod tests {
    use super::*;
    use orchestraitor_events::{
        AuditStore, CURRENT_SCHEMA_VERSION, EventCategory, EventEnvelope, EventEnvelopeInput,
        InMemoryAuditStore, PrivacyExportMode,
    };
    use orchestraitor_model::OperationId;
    use serde_json::json;
    use tempfile::TempDir;

    fn make_event(seq: u64, category: EventCategory, payload: serde_json::Value) -> EventEnvelope {
        EventEnvelope::try_new(EventEnvelopeInput {
            schema_version: CURRENT_SCHEMA_VERSION,
            monotonic_seq: seq,
            wall_clock_ts: "2026-07-30T00:00:00Z".to_string(),
            correlation_id: OperationId::from_string("op_test".to_string()),
            parent_op_id: None,
            category,
            payload,
            prev_hash: None,
        })
        .unwrap()
    }

    #[test]
    fn redacted_export_strips_prompt_payloads() {
        let mut store = InMemoryAuditStore::default();
        store
            .append(make_event(
                1,
                EventCategory::ModelRequest,
                json!({"prompt": "secret prompt"}),
            ))
            .unwrap();

        let exported = store.export(PrivacyExportMode::Redacted).unwrap();
        let rendered = String::from_utf8(exported).unwrap();

        assert!(!rendered.contains("secret prompt"));
        assert!(rendered.contains("redacted"));
    }

    #[test]
    fn count_records_counts_newlines() {
        assert_eq!(count_records(b"line1\nline2\nline3\n"), 3);
        assert_eq!(count_records(b""), 0);
    }

    #[test]
    fn missing_event_store_returns_empty() -> miette::Result<()> {
        let temp = TempDir::new().unwrap();
        let paths = ConfigPaths {
            config_dir: temp.path().to_path_buf(),
            project_dir: temp.path().to_path_buf(),
            models_dev_endpoint: None,
        };
        let store = load_event_store(&paths, None)?;
        assert!(store.records().is_empty());
        Ok(())
    }
}
