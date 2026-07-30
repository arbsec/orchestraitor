//! `tracing_subscriber` layer that emits canonical JSON Lines audit records.

use std::io::Write;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::time::{SystemTime, UNIX_EPOCH};

use orchestraitor_core::is_redacted_field;
use orchestraitor_model::OperationId;
use serde_json::{Map, Value};
use tracing::{Event, Subscriber, field::Visit};
use tracing_subscriber::{Layer, layer::Context};

use crate::{
    AuditRecord, CURRENT_SCHEMA_VERSION, EventCategory, EventEnvelope, EventEnvelopeInput,
    HashDigest,
};

/// Tracing layer that writes canonical JSON Lines audit records without unbounded buffering.
#[derive(Debug)]
pub struct TracingAuditLayer<W> {
    writer: Arc<Mutex<W>>,
    next_sequence: Arc<AtomicU64>,
    previous_hash: Arc<Mutex<Option<HashDigest>>>,
}

impl<W> Clone for TracingAuditLayer<W> {
    fn clone(&self) -> Self {
        Self {
            writer: Arc::clone(&self.writer),
            next_sequence: Arc::clone(&self.next_sequence),
            previous_hash: Arc::clone(&self.previous_hash),
        }
    }
}

impl<W> TracingAuditLayer<W>
where
    W: Write,
{
    /// Creates a tracing audit layer that writes directly to the supplied sink.
    #[must_use]
    pub fn new(writer: W) -> Self {
        Self {
            writer: Arc::new(Mutex::new(writer)),
            next_sequence: Arc::new(AtomicU64::new(1)),
            previous_hash: Arc::new(Mutex::new(None)),
        }
    }
}

impl TracingAuditLayer<Vec<u8>> {
    /// Returns captured bytes for test sinks.
    #[must_use]
    pub fn bytes(&self) -> Vec<u8> {
        match self.writer.lock() {
            Ok(writer) => writer.clone(),
            Err(_) => Vec::new(),
        }
    }
}

impl<S, W> Layer<S> for TracingAuditLayer<W>
where
    S: Subscriber,
    W: Write + Send + 'static,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut visitor = JsonVisitor::default();
        event.record(&mut visitor);
        let sequence = self.next_sequence.fetch_add(1, Ordering::Relaxed);
        let previous_hash = match self.previous_hash.lock() {
            Ok(guard) => guard.clone(),
            Err(_) => None,
        };
        let input = EventEnvelopeInput {
            schema_version: CURRENT_SCHEMA_VERSION,
            monotonic_seq: sequence,
            wall_clock_ts: wall_clock_timestamp(),
            correlation_id: OperationId::from_string("trace".to_string()),
            parent_op_id: None,
            category: category_for_event(event),
            payload: Value::Object(visitor.fields),
            prev_hash: previous_hash,
        };
        let Ok(envelope) = EventEnvelope::try_new(input) else {
            return;
        };
        let Ok(record) = AuditRecord::try_from_envelope(envelope) else {
            return;
        };
        if let Ok(mut previous_hash_guard) = self.previous_hash.lock() {
            *previous_hash_guard = Some(record.hash.clone());
        }
        if let Ok(mut writer) = self.writer.lock() {
            let _write_result = serde_json_canonicalizer::to_writer(&record, &mut *writer);
            let _newline_result = writer.write_all(b"\n");
        }
    }
}

#[derive(Debug, Default)]
struct JsonVisitor {
    fields: Map<String, Value>,
}

impl Visit for JsonVisitor {
    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        self.record_value(field, Value::from(value));
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.record_value(field, Value::from(value));
    }

    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.record_value(field, Value::from(value));
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.record_value(field, Value::from(value));
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.record_value(field, Value::from(format!("{value:?}")));
    }
}

impl JsonVisitor {
    fn record_value(&mut self, field: &tracing::field::Field, value: Value) {
        let name = field.name();
        if !is_redacted_field(name) {
            self.fields.insert(name.to_string(), value);
        }
    }
}

fn category_for_event(event: &Event<'_>) -> EventCategory {
    if *event.metadata().level() == tracing::Level::ERROR {
        EventCategory::Error
    } else {
        EventCategory::ResourceUsage
    }
}

fn wall_clock_timestamp() -> String {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => format!("{}.{:09}Z", duration.as_secs(), duration.subsec_nanos()),
        Err(_) => "0.000000000Z".to_string(),
    }
}
