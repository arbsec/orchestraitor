//! Tests for daemon `SQLite` WAL store and filesystem CAS.

#![allow(clippy::unwrap_used)]

use orchestraitor_events::{
    CURRENT_SCHEMA_VERSION, EventCategory, EventEnvelope, EventEnvelopeInput, EventError,
    EventQuery,
};
use orchestraitor_model::OperationId;
use serde_json::json;

use crate::store::{DaemonStore, StoreError, StorePaths, StoreResult};

#[test]
fn store_initializes_sqlite_wal_schema() {
    let temp = tempfile::tempdir().unwrap();
    let paths = paths(temp.path());

    let store = DaemonStore::open(&paths).unwrap();

    assert_eq!(store.journal_mode().unwrap(), "wal");
    assert!(paths.cas_root.join("objects").is_dir());
}

#[test]
fn event_records_persist_across_restart() -> Result<(), EventError> {
    let temp = tempfile::tempdir().unwrap();
    let paths = paths(temp.path());
    let first_hash = {
        let store = DaemonStore::open(&paths).unwrap();
        let first = event(
            1,
            EventCategory::SessionLifecycle,
            json!({"state":"started"}),
            None,
        )?;
        store.append_event(first).unwrap().hash
    };

    let store = DaemonStore::open(&paths).unwrap();
    let second = event(
        2,
        EventCategory::ToolRequest,
        json!({"tool":"read"}),
        Some(first_hash),
    )?;
    store.append_event(second).unwrap();
    let records = store.load_event_records().unwrap();

    assert_eq!(records.len(), 2);
    assert_eq!(records[1].envelope.category, EventCategory::ToolRequest);
    Ok(())
}

#[test]
fn hash_chain_rejects_wrong_previous_hash() -> Result<(), EventError> {
    let temp = tempfile::tempdir().unwrap();
    let store = DaemonStore::open_in_memory(temp.path()).unwrap();
    let first = event(1, EventCategory::SessionLifecycle, json!({}), None)?;
    store.append_event(first).unwrap();
    let bad = event(2, EventCategory::ToolRequest, json!({"tool":"write"}), None)?;

    let result = store.append_event(bad);

    assert!(result.is_err());
    Ok(())
}

#[test]
fn cas_stores_and_retrieves_by_digest() {
    let temp = tempfile::tempdir().unwrap();
    let store = DaemonStore::open_in_memory(temp.path()).unwrap();
    let bytes = b"orchestraitor daemon blob";

    let digest = store.cas().store_bytes(bytes).unwrap();
    let loaded = store.cas().load_bytes(&digest).unwrap();

    assert_eq!(loaded, bytes);
    assert!(store.cas().path_for_digest(&digest).unwrap().is_file());
}

#[test]
fn cas_load_bytes_rejects_corrupted_blob() {
    let temp = tempfile::tempdir().unwrap();
    let store = DaemonStore::open_in_memory(temp.path()).unwrap();
    let bytes = b"orchestraitor daemon blob";
    let digest = store.cas().store_bytes(bytes).unwrap();

    // Simulate disk corruption or an out-of-band write that left the blob
    // under its address but with a different payload. `load_bytes` must
    // refuse to return bytes whose SHA-256 does not match the address.
    let path = store.cas().path_for_digest(&digest).unwrap();
    std::fs::write(&path, b"corrupted contents").unwrap();

    let result = store.cas().load_bytes(&digest);
    assert!(
        matches!(
            result,
            Err(StoreError::DigestMismatch {
                expected: _,
                actual: _
            })
        ),
        "expected DigestMismatch, got {result:?}"
    );
}

#[test]
fn load_event_records_rejects_tampered_record_json() -> StoreResult<()> {
    let temp = tempfile::tempdir().unwrap();
    let store = DaemonStore::open_in_memory(temp.path()).unwrap();
    store.append_event(event(
        1,
        EventCategory::SessionLifecycle,
        json!({"state":"started"}),
        None,
    )?)?;

    // Out-of-band write to the persisted JSON column mutates the envelope
    // payload while keeping the original claimed hash. Recanonicalizing the
    // envelope must no longer match the stored hash.
    let tampered = r#"{"envelope":{"schema_version":1,"monotonic_seq":1,"wall_clock_ts":"2026-07-30T00:00:00Z","correlation_id":"op_daemon_store_test","parent_op_id":null,"category":"session_lifecycle","payload":{"state":"tampered"},"prev_hash":null},"hash":"0000000000000000000000000000000000000000000000000000000000000000"}"#;
    store.execute_raw(&format!(
        "UPDATE event_records SET record_json = '{tampered}' WHERE monotonic_seq = 1"
    ))?;

    let result = store.load_event_records();
    assert!(
        matches!(
            result,
            Err(StoreError::Event(EventError::RecordHashMismatch {
                sequence: 1
            }))
        ),
        "expected RecordHashMismatch at sequence 1, got {result:?}"
    );
    Ok(())
}

#[test]
fn load_event_records_rejects_record_json_payload_drift() -> StoreResult<()> {
    let temp = tempfile::tempdir().unwrap();
    let store = DaemonStore::open_in_memory(temp.path()).unwrap();
    let first = store.append_event(event(
        1,
        EventCategory::SessionLifecycle,
        json!({"state":"started"}),
        None,
    )?)?;
    store.append_event(event(
        2,
        EventCategory::ToolRequest,
        json!({"tool":"read"}),
        Some(first.hash.clone()),
    )?)?;

    let original = first.hash.as_str().to_owned();
    let drift = format!(
        r#"{{"envelope":{{"schema_version":1,"monotonic_seq":1,"wall_clock_ts":"2026-07-30T00:00:00Z","correlation_id":"op_daemon_store_test","parent_op_id":null,"category":"session_lifecycle","payload":{{"state":"hijacked"}},"prev_hash":null}},"hash":"{original}"}}"#
    );
    store.execute_raw(&format!(
        "UPDATE event_records SET record_json = '{drift}' WHERE monotonic_seq = 1"
    ))?;

    let result = store.load_event_records();
    assert!(
        matches!(
            result,
            Err(StoreError::Event(EventError::RecordHashMismatch {
                sequence: 1
            }))
        ),
        "expected RecordHashMismatch at sequence 1, got {result:?}"
    );
    Ok(())
}

#[test]
fn query_events_filters_by_category() -> StoreResult<()> {
    let temp = tempfile::tempdir().unwrap();
    let store = DaemonStore::open_in_memory(temp.path()).unwrap();
    let first = store.append_event(event(
        1,
        EventCategory::SessionLifecycle,
        json!({"state":"started"}),
        None,
    )?)?;
    store.append_event(event(
        2,
        EventCategory::PolicyDecision,
        json!({"verdict":"pass"}),
        Some(first.hash),
    )?)?;

    let events = store.query_events(&EventQuery {
        category: Some(EventCategory::PolicyDecision),
        include_uninterpreted: true,
        ..EventQuery::default()
    })?;

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].envelope.monotonic_seq, 2);
    Ok(())
}

fn paths(root: &std::path::Path) -> StorePaths {
    StorePaths {
        database_path: root.join("daemon.sqlite3"),
        cas_root: root.join("cas"),
    }
}

fn event(
    sequence: u64,
    category: EventCategory,
    payload: serde_json::Value,
    prev_hash: Option<orchestraitor_events::HashDigest>,
) -> Result<EventEnvelope, EventError> {
    EventEnvelope::try_new(EventEnvelopeInput {
        schema_version: CURRENT_SCHEMA_VERSION,
        monotonic_seq: sequence,
        wall_clock_ts: "2026-07-30T00:00:00Z".to_string(),
        correlation_id: OperationId::from_string("op_daemon_store_test".to_string()),
        parent_op_id: None,
        category,
        payload,
        prev_hash,
    })
}
