//! Privacy-preserving event export redaction.

use orchestraitor_core::is_redacted_field;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::{AuditRecord, EventCategory, EventEnvelope};

/// Privacy mode for exported event records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivacyExportMode {
    /// Preserve payloads except fields that are always sensitive.
    Full,
    /// Redact high-risk payload classes while preserving replay metadata.
    Redacted,
}

/// Returns a redacted copy of an event envelope for privacy-preserving export.
#[must_use]
pub fn redact_event(record: &AuditRecord, mode: PrivacyExportMode) -> EventEnvelope {
    let envelope = &record.envelope;
    let mut redacted = envelope.clone();
    redacted.payload = if redacts_entire_payload(mode, envelope.category) {
        json!({"redacted": true})
    } else {
        redact_sensitive_fields(&envelope.payload)
    };
    redacted
}

fn redacts_entire_payload(mode: PrivacyExportMode, category: EventCategory) -> bool {
    match (mode, category) {
        (_, EventCategory::SecretUse)
        | (
            PrivacyExportMode::Redacted,
            EventCategory::ModelRequest
            | EventCategory::ModelResponseMetadata
            | EventCategory::ContextSelection
            | EventCategory::ToolRequest
            | EventCategory::ActionPlan
            | EventCategory::FileObservation,
        ) => true,
        (
            PrivacyExportMode::Full,
            EventCategory::SessionLifecycle
            | EventCategory::AdapterLifecycle
            | EventCategory::ModelRequest
            | EventCategory::ModelResponseMetadata
            | EventCategory::ContextSelection
            | EventCategory::ToolRequest
            | EventCategory::ActionPlan
            | EventCategory::PolicyDecision
            | EventCategory::Approval
            | EventCategory::ProcessExecution
            | EventCategory::NetworkRequest
            | EventCategory::FileObservation
            | EventCategory::GitOperation
            | EventCategory::OutputPromotion
            | EventCategory::SandboxCapability
            | EventCategory::ResourceUsage
            | EventCategory::Error
            | EventCategory::SecurityFinding,
        )
        | (
            PrivacyExportMode::Redacted,
            EventCategory::SessionLifecycle
            | EventCategory::AdapterLifecycle
            | EventCategory::PolicyDecision
            | EventCategory::Approval
            | EventCategory::ProcessExecution
            | EventCategory::NetworkRequest
            | EventCategory::GitOperation
            | EventCategory::OutputPromotion
            | EventCategory::SandboxCapability
            | EventCategory::ResourceUsage
            | EventCategory::Error
            | EventCategory::SecurityFinding,
        ) => false,
    }
}

fn redact_sensitive_fields(value: &Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(redact_object(object)),
        Value::Array(items) => Value::Array(items.iter().map(redact_sensitive_fields).collect()),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => value.clone(),
    }
}

fn redact_object(object: &Map<String, Value>) -> Map<String, Value> {
    object
        .iter()
        .map(|(key, value)| {
            let replacement = if is_redacted_field(key) {
                json!({"redacted": true})
            } else {
                redact_sensitive_fields(value)
            };
            (key.clone(), replacement)
        })
        .collect()
}
