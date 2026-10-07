//! Tests for the SQLite-backed audit store, mirroring in-memory semantics.

#![allow(clippy::unwrap_used)]

use orchestraitor_model::OperationId;
use serde_json::{Value, json};

use orchestraitor_events::{
    AuditStore, CURRENT_SCHEMA_VERSION, EventCategory, EventEnvelope, EventEnvelopeInput,
    EventError, EventQuery, InMemoryAuditStore, PrivacyExportMode, SqliteAuditStore, hash_envelope,
};

#[test]
fn sqlite_round_trip_matches_in_memory_semantics() -> Result<(), EventError> {
    let mut sqlite = SqliteAuditStore::open_in_memory()?;
    let mut memory = InMemoryAuditStore::default();

    for (sequence, category, payload) in [
        (
            1,
            EventCategory::SessionLifecycle,
            json!({"state":"started"}),
        ),
        (2, EventCategory::ToolRequest, json!({"tool":"read"})),
        (
            3,
            EventCategory::GitOperation,
            json!({"operation":"status"}),
        ),
    ] {
        let envelope = event(sequence, category, payload, memory.head()?.prev_hash)?;
        memory.append(envelope.clone())?;
        sqlite.append(envelope)?;
    }

    let memory_records = memory.records();
    let sqlite_records = sqlite.query(&EventQuery {
        include_uninterpreted: true,
        ..EventQuery::default()
    })?;
    assert_eq!(memory_records, sqlite_records.as_slice());

    let query = EventQuery {
        category: Some(EventCategory::ToolRequest),
        since_seq: Some(2),
        until_seq: Some(2),
        include_uninterpreted: true,
    };
    assert_eq!(
        sqlite.query(&query)?,
        memory.query(&query)?.as_slice(),
        "filtered queries must match in-memory semantics"
    );

    let exported_sqlite = sqlite.export(PrivacyExportMode::Full)?;
    let exported_memory = memory.export(PrivacyExportMode::Full)?;
    assert_eq!(exported_sqlite, exported_memory);

    // Redacted-export parity: redaction + chain relinking must produce
    // byte-identical output on both backends.
    let redacted_sqlite = sqlite.export(PrivacyExportMode::Redacted)?;
    let redacted_memory = memory.export(PrivacyExportMode::Redacted)?;
    assert_eq!(redacted_sqlite, redacted_memory);

    let mut reimported_sqlite = SqliteAuditStore::open_in_memory()?;
    let imported = reimported_sqlite.r#import(&exported_sqlite)?;
    assert_eq!(imported, memory_records.to_vec());
    assert_eq!(
        reimported_sqlite.query(&EventQuery {
            include_uninterpreted: true,
            ..EventQuery::default()
        })?,
        memory_records
    );

    // Importing the redacted export must relink the hash chain so the fresh
    // store validates and queries cleanly.
    let mut redacted_store = SqliteAuditStore::open_in_memory()?;
    let redacted_imported = redacted_store.r#import(&redacted_sqlite)?;
    assert_eq!(redacted_imported.len(), memory_records.len());
    let redacted_query = redacted_store.query(&EventQuery {
        include_uninterpreted: true,
        ..EventQuery::default()
    })?;
    assert_eq!(redacted_query.len(), memory_records.len());
    let relinked_head = redacted_store.head()?;
    assert_eq!(
        relinked_head.prev_hash,
        redacted_query.last().map(|record| record.hash.clone())
    );
    assert!(orchestraitor_events::validate_hash_chain(&redacted_imported).is_ok());
    Ok(())
}

#[test]
fn duplicate_sequence_append_is_rejected_and_store_unchanged() -> Result<(), EventError> {
    let mut store = SqliteAuditStore::open_in_memory()?;
    store.append(event(1, EventCategory::SessionLifecycle, json!({}), None)?)?;

    let before = count(&store);
    let result = store.append(event(1, EventCategory::SessionLifecycle, json!({}), None)?);

    assert!(
        matches!(
            result,
            Err(EventError::SequenceGap {
                expected: 2,
                observed: 1
            })
        ),
        "duplicate sequence must map to a chain error, got {result:?}"
    );
    assert_eq!(
        count(&store),
        before,
        "FORBIDDEN EFFECT: rejected append mutated the store"
    );
    Ok(())
}

#[test]
fn chain_gap_import_is_rejected() -> Result<(), EventError> {
    let mut store = SqliteAuditStore::open_in_memory()?;
    store.append(event(1, EventCategory::SessionLifecycle, json!({}), None)?)?;
    let mut records = parse_lines(&store.export(PrivacyExportMode::Full)?)?;
    records[0].envelope.monotonic_seq = 7;

    let result = store.r#import(&json_lines(&records)?);

    assert!(matches!(result, Err(EventError::SequenceGap { .. })));
    assert_eq!(
        count(&store),
        1,
        "FORBIDDEN EFFECT: rejected import mutated the store"
    );
    Ok(())
}

#[test]
fn tampered_envelope_bytes_are_detected_on_read() -> Result<(), EventError> {
    let directory = tempfile::tempdir().map_err(io_error)?;
    let path = directory.path().join("audit.db");
    {
        let mut store = SqliteAuditStore::open(&path)?;
        store.append(event(
            1,
            EventCategory::SessionLifecycle,
            json!({"state":"started"}),
            None,
        )?)?;
    }

    // Tamper with the stored envelope bytes directly in SQLite, keeping the
    // recorded hash untouched: validation must recompute from the envelope.
    let connection = rusqlite::Connection::open(&path)?;
    let (envelope_json, stored_hash): (Vec<u8>, String) = connection.query_row(
        "SELECT envelope_json, hash FROM audit_records WHERE monotonic_seq = 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let original_json = envelope_json.clone();
    let tampered = String::from_utf8(envelope_json)
        .map_err(|error| io_error(std::io::Error::new(std::io::ErrorKind::InvalidData, error)))?
        .replace("\"started\"", "\"tampered\"");
    assert_ne!(tampered, String::from_utf8_lossy(&original_json).as_ref());
    connection.execute(
        "UPDATE audit_records SET envelope_json = ?1 WHERE monotonic_seq = 1",
        [tampered.as_bytes()],
    )?;
    drop(connection);
    assert_ne!(stored_hash, "");

    let reopened = SqliteAuditStore::open(&path)?;
    let result = reopened.query(&EventQuery {
        include_uninterpreted: true,
        ..EventQuery::default()
    });
    assert!(
        matches!(result, Err(EventError::RecordHashMismatch { sequence: 1 })),
        "tampered envelope bytes must be quarantined with a hash mismatch, got {result:?}"
    );
    Ok(())
}

#[test]
fn corrupted_metadata_column_is_detected_on_read() -> Result<(), EventError> {
    let directory = tempfile::tempdir().map_err(io_error)?;
    let path = directory.path().join("audit.db");
    {
        let mut store = SqliteAuditStore::open(&path)?;
        let first = store.append(event(1, EventCategory::SessionLifecycle, json!({}), None)?)?;
        store.append(event(
            2,
            EventCategory::ToolRequest,
            json!({"tool":"read"}),
            Some(first.hash),
        )?)?;
    }

    // Edit the stored `category` column directly, leaving the envelope bytes
    // and hash intact: the metadata/envelope divergence must be detected —
    // never silently trusted to filter rows.
    let connection = rusqlite::Connection::open(&path)?;
    let affected = connection.execute(
        "UPDATE audit_records SET category = ?1 WHERE monotonic_seq = 2",
        [serde_json::to_string(&EventCategory::GitOperation)?],
    )?;
    drop(connection);
    assert_eq!(
        affected, 1,
        "raw metadata corruption must modify exactly one row"
    );

    let reopened = SqliteAuditStore::open(&path)?;
    let result = reopened.query(&EventQuery {
        include_uninterpreted: true,
        ..EventQuery::default()
    });
    assert!(
        matches!(result, Err(EventError::RecordHashMismatch { sequence: 2 })),
        "corrupted metadata column must be quarantined, got {result:?}"
    );

    // Forbidden effect check: the tampered row must not silently surface or
    // vanish — every read of the store fails closed until repaired.
    let filtered = reopened.query(&EventQuery {
        category: Some(EventCategory::SessionLifecycle),
        include_uninterpreted: true,
        ..EventQuery::default()
    });
    assert!(
        matches!(
            filtered,
            Err(EventError::RecordHashMismatch { sequence: 2 })
        ),
        "category-filtered query must also fail closed, got {filtered:?}"
    );
    Ok(())
}

#[test]
fn future_schema_version_is_filtered_by_include_uninterpreted() -> Result<(), EventError> {
    let mut store = SqliteAuditStore::open_in_memory()?;
    let mut envelope = event(1, EventCategory::ResourceUsage, json!({"cpu_ms":1}), None)?;
    envelope.schema_version = CURRENT_SCHEMA_VERSION.saturating_add(1);
    store.append(envelope)?;

    let hidden = store.query(&EventQuery::default())?;
    assert!(
        hidden.is_empty(),
        "future schema must stay uninterpreted by default"
    );
    let visible = store.query(&EventQuery {
        include_uninterpreted: true,
        ..EventQuery::default()
    })?;
    assert_eq!(visible.len(), 1);
    Ok(())
}

#[test]
fn file_backed_store_persists_across_reopen() -> Result<(), EventError> {
    let directory = tempfile::tempdir().map_err(io_error)?;
    let path = directory.path().join("audit.db");

    {
        let mut store = SqliteAuditStore::open(&path)?;
        let first = store.append(event(1, EventCategory::SessionLifecycle, json!({}), None)?)?;
        store.append(event(
            2,
            EventCategory::ToolRequest,
            json!({"tool":"read"}),
            Some(first.hash),
        )?)?;
    }

    let reopened = SqliteAuditStore::open(&path)?;
    let records = reopened.query(&EventQuery {
        include_uninterpreted: true,
        ..EventQuery::default()
    })?;
    assert_eq!(records.len(), 2, "records must survive close/reopen");
    let head = reopened.head()?;
    assert_eq!(head.seq_base, 2);
    assert_eq!(head.prev_hash, Some(records[1].hash.clone()));
    Ok(())
}

#[test]
fn deleted_middle_row_fails_append_not_just_read() -> Result<(), EventError> {
    let directory = tempfile::tempdir().map_err(io_error)?;
    let path = directory.path().join("audit.db");
    {
        let mut store = SqliteAuditStore::open(&path)?;
        let first = store.append(event(1, EventCategory::SessionLifecycle, json!({}), None)?)?;
        store.append(event(
            2,
            EventCategory::ToolRequest,
            json!({"tool":"read"}),
            Some(first.hash),
        )?)?;
    }

    // Delete the middle row directly: the last row is still a self-consistent
    // envelope/hash pair, so a highest-sequence-only head read would validate
    // and append on top of a broken chain.
    let connection = rusqlite::Connection::open(&path)?;
    let affected = connection.execute("DELETE FROM audit_records WHERE monotonic_seq = 1", [])?;
    drop(connection);
    assert_eq!(affected, 1, "row deletion must modify exactly one row");

    let mut reopened = SqliteAuditStore::open(&path)?;
    let deleted_first_hash =
        hash_envelope(&event(1, EventCategory::SessionLifecycle, json!({}), None)?)?;
    let append = reopened.append(event(
        3,
        EventCategory::GitOperation,
        json!({"op":"commit"}),
        // prev_hash must reference the now-deleted first record to make the
        // intent unambiguous: appending on top of the highest surviving row.
        Some(deleted_first_hash),
    )?);
    assert!(
        matches!(
            append,
            Err(EventError::SequenceGap {
                expected: 1,
                observed: 2
            })
        ),
        "append must validate the full chain, not just the highest row; the \
         deleted first row breaks the chain anchor and must fail closed, got {append:?}"
    );
    // Forbidden effect check: nothing persisted despite the failed append.
    // `query` fails closed on the broken chain, so count the raw rows.
    let raw_rows: i64 = rusqlite::Connection::open(&path)?.query_row(
        "SELECT COUNT(*) FROM audit_records",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(raw_rows, 1, "failed append must leave the store untouched");
    Ok(())
}

fn count(store: &SqliteAuditStore) -> usize {
    store
        .query(&EventQuery {
            include_uninterpreted: true,
            ..EventQuery::default()
        })
        .map_or(0, |records| records.len())
}

fn event(
    sequence: u64,
    category: EventCategory,
    payload: Value,
    prev_hash: Option<orchestraitor_events::HashDigest>,
) -> Result<EventEnvelope, EventError> {
    EventEnvelope::try_new(EventEnvelopeInput {
        schema_version: CURRENT_SCHEMA_VERSION,
        monotonic_seq: sequence,
        wall_clock_ts: "2026-07-30T00:00:00Z".to_string(),
        correlation_id: OperationId::from_string("op_sqlite_test".to_string()),
        parent_op_id: None,
        category,
        payload,
        prev_hash,
    })
}

fn parse_lines(bytes: &[u8]) -> Result<Vec<orchestraitor_events::AuditRecord>, EventError> {
    let mut records = Vec::new();
    for line in bytes.split(|byte| *byte == b'\n') {
        if line.is_empty() {
            continue;
        }
        records.push(serde_json::from_slice(line)?);
    }
    Ok(records)
}

fn json_lines(records: &[orchestraitor_events::AuditRecord]) -> Result<Vec<u8>, EventError> {
    let mut output = Vec::new();
    for record in records {
        serde_json::to_writer(&mut output, record)?;
        output.push(b'\n');
    }
    Ok(output)
}

fn io_error(error: std::io::Error) -> EventError {
    EventError::Json(serde_json::Error::io(error))
}
