//! Tests for daemon `SQLite` WAL store and filesystem CAS.

#![allow(clippy::unwrap_used)]

use orchestraitor_events::{
    CURRENT_SCHEMA_VERSION, EventCategory, EventEnvelope, EventEnvelopeInput, EventError,
    EventQuery,
};
use orchestraitor_model::OperationId;
use serde_json::json;

use crate::store::{DaemonStore, StorePaths, StoreResult};

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
