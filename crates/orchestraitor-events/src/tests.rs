//! Tests for normalized event records and tamper-evident imports.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use orchestraitor_model::OperationId;
use serde_json::{Value, json};

use crate::{
    AuditRecord, AuditStore, CURRENT_SCHEMA_VERSION, EventCategory, EventEnvelope,
    EventEnvelopeInput, EventError, EventQuery, InMemoryAuditStore, PrivacyExportMode,
    SchemaInterpretation, hash_envelope, validate_hash_chain,
};

#[test]
fn event_round_trips_with_canonical_record_hash() -> Result<(), EventError> {
    let event = event(
        1,
        EventCategory::SessionLifecycle,
        json!({"state":"started"}),
        None,
    )?;

    let record = AuditRecord::try_from_envelope(event)?;
    let encoded = serde_json::to_vec(&record)?;
    let decoded: AuditRecord = serde_json::from_slice(&encoded)?;

    assert_eq!(record, decoded);
    assert_eq!(decoded.envelope.schema_version, CURRENT_SCHEMA_VERSION);
    Ok(())
}

#[test]
fn hash_chain_validation_accepts_contiguous_records() -> Result<(), EventError> {
    let mut store = InMemoryAuditStore::default();
    let first = event(
        1,
        EventCategory::SessionLifecycle,
        json!({"state":"started"}),
        None,
    )?;
    let first_record = store.append(first)?;
    let second = event(
        2,
        EventCategory::GitOperation,
        json!({"operation":"status"}),
        Some(first_record.hash),
    )?;
    store.append(second)?;

    validate_hash_chain(store.records())?;
    assert_eq!(store.records().len(), 2);
    Ok(())
}

#[test]
fn redaction_guard_rejects_api_key_field() {
    let result = event(
        1,
        EventCategory::ToolRequest,
        json!({"api_key":"secret"}),
        None,
    );

    assert!(matches!(result, Err(EventError::SensitiveField { field }) if field == "api_key"));
}

#[test]
fn tamper_detection_rejects_imported_record_mutation() -> Result<(), EventError> {
    let mut store = InMemoryAuditStore::default();
    let first = event(
        1,
        EventCategory::SessionLifecycle,
        json!({"state":"started"}),
        None,
    )?;
    let first_record = store.append(first)?;
    let second = event(
        2,
        EventCategory::ToolRequest,
        json!({"tool":"read"}),
        Some(first_record.hash),
    )?;
    store.append(second)?;
    let exported = store.export(PrivacyExportMode::Full)?;
    let mut records = parse_lines(&exported)?;
    records[1].envelope.payload = json!({"tool":"write"});
    let tampered = json_lines(&records)?;
    let mut imported = InMemoryAuditStore::default();

    let result = imported.r#import(&tampered);

    assert!(matches!(
        result,
        Err(EventError::RecordHashMismatch { sequence: 2 })
    ));
    Ok(())
}

#[test]
fn export_redacts_prompt_like_payloads_and_secret_events() -> Result<(), EventError> {
    let mut store = InMemoryAuditStore::default();
    let request = event(
        1,
        EventCategory::ModelRequest,
        json!({"prompt":"hello"}),
        None,
    )?;
    let request_record = store.append(request)?;
    let secret = event(
        2,
        EventCategory::SecretUse,
        json!({"secret_ref":"secret://env/API"}),
        Some(request_record.hash),
    )?;
    store.append(secret)?;

    let exported = store.export(PrivacyExportMode::Redacted)?;
    let rendered = String::from_utf8(exported).map_err(|error| {
        EventError::Json(serde_json::Error::io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            error,
        )))
    })?;

    assert!(!rendered.contains("hello"));
    assert!(!rendered.contains("secret://env/API"));
    assert!(rendered.contains("redacted"));
    let mut imported = InMemoryAuditStore::default();
    imported.r#import(rendered.as_bytes())?;
    Ok(())
}

#[test]
fn future_schema_is_preserved_as_uninterpreted() -> Result<(), EventError> {
    let mut store = InMemoryAuditStore::default();
    let future = EventEnvelope::try_new(EventEnvelopeInput {
        schema_version: CURRENT_SCHEMA_VERSION.saturating_add(1),
        monotonic_seq: 1,
        wall_clock_ts: "2026-07-30T00:00:00Z".to_string(),
        correlation_id: OperationId::from_string("op_future".to_string()),
        parent_op_id: None,
        category: EventCategory::ResourceUsage,
        payload: json!({"cpu_ms":1}),
        prev_hash: None,
    })?;
    store.append(future)?;

    let hidden = store.query(&EventQuery::default())?;
    let visible = store.query(&EventQuery {
        include_uninterpreted: true,
        ..EventQuery::default()
    })?;

    assert_eq!(hidden, [] as [AuditRecord; 0]);
    assert_eq!(visible.len(), 1);
    Ok(())
}

#[test]
fn audit_record_schema_interpretation_is_derived_from_envelope() -> Result<(), EventError> {
    let interpreted = AuditRecord::try_from_envelope(event(
        1,
        EventCategory::SessionLifecycle,
        json!({}),
        None,
    )?)?;
    assert_eq!(
        interpreted.schema_interpretation(),
        SchemaInterpretation::Interpreted
    );
    assert_eq!(
        hash_envelope(&interpreted.envelope)?,
        interpreted.hash,
        "hash must cover every envelope byte"
    );

    let future = AuditRecord::try_from_envelope(EventEnvelope::try_new(EventEnvelopeInput {
        schema_version: CURRENT_SCHEMA_VERSION.saturating_add(1),
        monotonic_seq: 2,
        wall_clock_ts: "2026-07-30T00:00:00Z".to_string(),
        correlation_id: OperationId::from_string("op_future".to_string()),
        parent_op_id: None,
        category: EventCategory::ResourceUsage,
        payload: json!({"cpu_ms": 1}),
        prev_hash: Some(interpreted.hash),
    })?)?;
    assert_eq!(
        future.schema_interpretation(),
        SchemaInterpretation::Uninterpreted
    );
    Ok(())
}

#[test]
fn schema_version_tampering_invalidates_hash_chain() -> Result<(), EventError> {
    let mut store = InMemoryAuditStore::default();
    store.append(event(
        1,
        EventCategory::SessionLifecycle,
        json!({"state": "started"}),
        None,
    )?)?;
    let mut records = parse_lines(&store.export(PrivacyExportMode::Full)?)?;
    records[0].envelope.schema_version = CURRENT_SCHEMA_VERSION.saturating_add(1);

    let result = validate_hash_chain(&records);
    assert!(
        matches!(result, Err(EventError::RecordHashMismatch { sequence: 1 })),
        "flipping schema_version must break the hash chain, got {result:?}"
    );
    Ok(())
}

#[test]
fn sensitive_field_names_reject_exact_matches() {
    for (field, label) in [
        ("secret", "exact `secret`"),
        ("token", "exact `token`"),
        ("password", "exact `password`"),
        ("cookie", "exact `cookie`"),
        (
            "Session-Token",
            "case/hyphen normalization to `session_token`",
        ),
        ("Auth-Token", "case/hyphen normalization to `auth_token`"),
    ] {
        let result = event(1, EventCategory::ToolRequest, json!({field: "value"}), None);
        assert!(
            matches!(result, Err(EventError::SensitiveField { field: ref found }) if found == field),
            "{label} (field `{field}`) must be rejected before persistence"
        );
    }
}

#[test]
fn tracing_layer_chain_validates_under_concurrent_emission() -> Result<(), EventError> {
    use std::sync::Arc;
    use std::thread;
    use tracing_subscriber::{Registry, layer::SubscriberExt};

    const THREADS: usize = 8;
    const EVENTS_PER_THREAD: usize = 25;

    let layer = crate::TracingAuditLayer::new(Vec::<u8>::new());
    let captured = layer.clone();
    let subscriber = Arc::new(Registry::default().with(layer));

    let handles: Vec<_> = (0..THREADS)
        .map(|thread_index| {
            let subscriber = Arc::clone(&subscriber);
            thread::spawn(move || {
                tracing::subscriber::with_default(subscriber, || {
                    for event_index in 0..EVENTS_PER_THREAD {
                        tracing::info!(thread = thread_index, event = event_index, "audit tick");
                    }
                });
            })
        })
        .collect();

    for handle in handles {
        handle.join().ok();
    }

    let bytes = captured.bytes();
    assert!(
        !bytes.is_empty(),
        "concurrent emission must produce records"
    );

    let records = parse_lines(&bytes)?;
    assert_eq!(
        records.len(),
        THREADS * EVENTS_PER_THREAD,
        "every emitted event must land in the sink"
    );

    validate_hash_chain(&records)?;
    Ok(())
}

#[test]
fn concurrent_import_during_append_never_persists_broken_chain() -> Result<(), EventError> {
    use std::sync::atomic::{AtomicBool, Ordering};

    use rusqlite::hooks::{AuthAction, AuthContext, Authorization};

    use crate::SqliteAuditStore;

    let directory = tempfile::tempdir()
        .map_err(|error| EventError::Json(serde_json::Error::io(std::io::Error::other(error))))?;
    let path = directory.path().join("audit.db");

    // Seed the database and remember the old head hash.
    let old_head;
    {
        let mut store = SqliteAuditStore::open(&path)?;
        let first = store.append(event(1, EventCategory::SessionLifecycle, json!({}), None)?)?;
        old_head = first.hash;
    }

    // A divergent, independently valid chain that replaces the history.
    let mut divergent = InMemoryAuditStore::default();
    divergent.append(event(
        1,
        EventCategory::SessionLifecycle,
        json!({"state":"diverged"}),
        None,
    )?)?;
    let divergent_bytes = divergent.export(PrivacyExportMode::Full)?;

    // Connection A gets an authorizer that, exactly when A prepares its
    // INSERT (after the head snapshot and chain validation), runs a history
    // replacement on a second connection — the interleaving CodeRabbit
    // flagged: B imports between A's head validation and A's insert.
    let b_ran = std::sync::Arc::new(AtomicBool::new(false));
    let b_ran_hook = std::sync::Arc::clone(&b_ran);
    let hook_path = path.clone();
    let hook = move |context: AuthContext<'_>| {
        if !matches!(context.action, AuthAction::Insert { .. })
            || b_ran_hook.swap(true, Ordering::SeqCst)
        {
            return Authorization::Allow;
        }
        // Run B's import from inside A's append. Short busy timeout: under
        // the serialized fix, B must lose the race to A's write lock here.
        let mut store_b = SqliteAuditStore::open(&hook_path).expect("open rival store");
        store_b
            .connection()
            .busy_timeout(std::time::Duration::from_millis(200))
            .expect("set busy timeout");
        let import = AuditStore::r#import(&mut store_b, &divergent_bytes);
        // The forbidden effect only occurs when B's replacement commits while
        // A is mid-append; either outcome below is acceptable as long as the
        // persisted chain stays valid (asserted after the append).
        let _ = import;
        Authorization::Allow
    };

    let mut store_a = SqliteAuditStore::open(&path)?;
    store_a.connection().authorizer(Some(hook))?;

    // A appends a record linked to the pre-import head while the hook injects
    // B's competing import at A's insert-preparation point.
    let append_result = store_a.append(event(
        2,
        EventCategory::ToolRequest,
        json!({"tool":"read"}),
        Some(old_head),
    )?);
    assert!(b_ran.load(Ordering::SeqCst), "hook must have fired");

    // FORBIDDEN EFFECT: whatever the race outcome, reopening the store must
    // load a fully valid chain. Before the serialization fix, B's import won
    // here and A's record was persisted with prev_hash pointing at the
    // removed pre-import head — a broken chain that survived reopen.
    let reopened = SqliteAuditStore::open(&path)?;
    let records = reopened.query(&EventQuery {
        include_uninterpreted: true,
        ..EventQuery::default()
    })?;
    match append_result {
        Ok(_) => assert_eq!(records.len(), 2, "committed append must persist"),
        Err(_) => assert_eq!(
            records.len(),
            1,
            "failed append must not leave a record behind"
        ),
    }
    if records.len() == 1 {
        assert_eq!(
            records[0].hash,
            divergent.head()?.prev_hash.unwrap(),
            "persisted history must be exactly the imported chain"
        );
    }
    Ok(())
}

fn event(
    sequence: u64,
    category: EventCategory,
    payload: Value,
    prev_hash: Option<crate::HashDigest>,
) -> Result<EventEnvelope, EventError> {
    EventEnvelope::try_new(EventEnvelopeInput {
        schema_version: CURRENT_SCHEMA_VERSION,
        monotonic_seq: sequence,
        wall_clock_ts: "2026-07-30T00:00:00Z".to_string(),
        correlation_id: OperationId::from_string("op_test".to_string()),
        parent_op_id: None,
        category,
        payload,
        prev_hash,
    })
}

fn parse_lines(bytes: &[u8]) -> Result<Vec<AuditRecord>, EventError> {
    let mut records = Vec::new();
    for line in bytes.split(|byte| *byte == b'\n') {
        if !line.is_empty() {
            records.push(serde_json::from_slice(line)?);
        }
    }
    Ok(records)
}

fn json_lines(records: &[AuditRecord]) -> Result<Vec<u8>, EventError> {
    let mut output = Vec::new();
    for record in records {
        serde_json_canonicalizer::to_writer(record, &mut output)
            .map_err(EventError::CanonicalJson)?;
        output.push(b'\n');
    }
    Ok(output)
}

#[test]
fn sqlite_cached_head_hit_fallback_and_invalidation() -> Result<(), EventError> {
    use crate::SqliteAuditStore;

    let directory = tempfile::tempdir()
        .map_err(|error| EventError::Json(serde_json::Error::io(std::io::Error::other(error))))?;
    let path = directory.path().join("audit.db");

    // 1. Two appends in a row: the second links to the first's hash — the
    //    cache-hit path (verified_head_for_append returns the cached head).
    let mut store = SqliteAuditStore::open(&path)?;
    let first = store.append(event(1, EventCategory::SessionLifecycle, json!({}), None)?)?;
    let second = store.append(event(
        2,
        EventCategory::SessionLifecycle,
        json!({"state":"second"}),
        Some(first.hash.clone()),
    )?)?;
    assert_eq!(second.envelope.prev_hash, Some(first.hash.clone()));

    // 2. A foreign connection edits the file: the next append must detect the
    //    data_version bump and re-verify from disk instead of trusting the
    //    stale cached head (the foreign write replaced the history).
    {
        let mut rival = SqliteAuditStore::open(&path)?;
        rival.r#import(&json_lines(&[{
            let mut divergent = InMemoryAuditStore::default();
            divergent.append(event(
                1,
                EventCategory::SessionLifecycle,
                json!({"state":"diverged"}),
                None,
            )?)?;
            divergent.export(PrivacyExportMode::Full)?;
            // Build one-record export: re-run through a fresh store.
            let mut one = InMemoryAuditStore::default();
            let record = one.append(event(
                1,
                EventCategory::SessionLifecycle,
                json!({"state":"diverged"}),
                None,
            )?)?;
            let _ = record;
            let mut one = InMemoryAuditStore::default();
            one.append(event(
                1,
                EventCategory::SessionLifecycle,
                json!({"state":"diverged"}),
                None,
            )?)?;
            let exported = one.export(PrivacyExportMode::Full)?;
            serde_json::from_slice::<AuditRecord>(
                exported.split(|b| *b == b'\n').next().unwrap_or(&[]),
            )?
        }])?)?;
    }

    // 3. The append after the foreign import must FAIL (sequence 3 does not
    //    follow the diverged chain's head), proving the stale cache was NOT
    //    trusted and the full re-verification path engaged.
    let stale_append = store.append(event(
        3,
        EventCategory::SessionLifecycle,
        json!({"state":"after-diverge"}),
        Some(second.hash.clone()),
    )?);
    // validate_next_envelope checks the SEQUENCE before prev_hash: the
    // imported (diverged) history ends at sequence 1, so appending
    // sequence 3 fails with SequenceGap { expected: 2 }.
    assert!(
        matches!(
            &stale_append,
            Err(EventError::SequenceGap {
                expected: 2,
                observed: 3,
            })
        ),
        "expected SequenceGap {{ expected: 2, observed: 3 }} (full re-verification must engage \
         after a foreign history replacement), got: {stale_append:?}"
    );
    Ok(())
}

#[test]
fn sqlite_cached_head_invalidates_on_foreign_append() -> Result<(), EventError> {
    use crate::SqliteAuditStore;

    let directory = tempfile::tempdir()
        .map_err(|error| EventError::Json(serde_json::Error::io(std::io::Error::other(error))))?;
    let path = directory.path().join("audit.db");

    // Two appends on connection A populate its cached head (seq 2).
    let mut store = SqliteAuditStore::open(&path)?;
    let first = store.append(event(1, EventCategory::SessionLifecycle, json!({}), None)?)?;
    let second = store.append(event(
        2,
        EventCategory::SessionLifecycle,
        json!({"state":"second"}),
        Some(first.hash.clone()),
    )?)?;

    // A foreign connection appends seq 3 (a legitimate foreign append, not an
    // import): the file advances past A's cached head.
    let mut rival = SqliteAuditStore::open(&path)?;
    let _third = rival.append(event(
        3,
        EventCategory::SessionLifecycle,
        json!({"state":"foreign"}),
        Some(second.hash.clone()),
    )?)?;

    // A's next append must NOT trust the cached head (seq 2): the row at the
    // cached seq still exists but is no longer MAX, so the full re-verification
    // path engages and validate_next_envelope expects sequence 4.
    let stale_append = store.append(event(
        3,
        EventCategory::SessionLifecycle,
        json!({"state":"after-foreign-append"}),
        Some(second.hash.clone()),
    )?);
    assert!(
        matches!(
            &stale_append,
            Err(EventError::SequenceGap {
                expected: 4,
                observed: 3,
            })
        ),
        "expected SequenceGap {{ expected: 4, observed: 3 }} (cached head must not be trusted \
         after a foreign append), got: {stale_append:?}"
    );
    Ok(())
}
