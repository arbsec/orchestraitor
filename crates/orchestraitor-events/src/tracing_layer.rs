//! `tracing_subscriber` layer that emits canonical JSON Lines audit records.

use std::io::Write;
use std::sync::{Arc, Mutex};
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

/// Hash-chain state protected by a single mutex.
///
/// Keeping sequence allocation, previous-hash read/update, and writer emission
/// under one lock prevents two concurrent events from advancing the sequence
/// while reading the same `previous_hash`, which would otherwise produce a
/// broken or duplicate chain.
#[derive(Debug)]
struct ChainState {
    /// Next sequence number to assign to an incoming event.
    next_sequence: u64,
    /// Hash of the most recently emitted record, used as the next `prev_hash`.
    previous_hash: Option<HashDigest>,
}

impl Default for ChainState {
    fn default() -> Self {
        Self {
            next_sequence: 1,
            previous_hash: None,
        }
    }
}

/// Tracing layer that writes canonical JSON Lines audit records without unbounded buffering.
#[derive(Debug)]
pub struct TracingAuditLayer<W> {
    chain: Arc<Mutex<ChainStateWriter<W>>>,
}

#[derive(Debug)]
struct ChainStateWriter<W> {
    state: ChainState,
    writer: W,
}

impl<W> Clone for TracingAuditLayer<W> {
    fn clone(&self) -> Self {
        Self {
            chain: Arc::clone(&self.chain),
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
            chain: Arc::new(Mutex::new(ChainStateWriter {
                state: ChainState::default(),
                writer,
            })),
        }
    }
}

impl TracingAuditLayer<Vec<u8>> {
    /// Returns captured bytes for test sinks.
    #[must_use]
    pub fn bytes(&self) -> Vec<u8> {
        match self.chain.lock() {
            Ok(guard) => guard.writer.clone(),
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

        // Hold the chain lock through sequence allocation, hashing, and write
        // emission so concurrent events cannot observe a stale `previous_hash`
        // and so records land on the sink in sequence order.
        let Ok(mut guard) = self.chain.lock() else {
            return;
        };
        let sequence = guard.state.next_sequence;
        let previous_hash = guard.state.previous_hash.clone();
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
        guard.state.next_sequence = guard.state.next_sequence.saturating_add(1);
        guard.state.previous_hash = Some(record.hash.clone());

        let _write_result = serde_json_canonicalizer::to_writer(&record, &mut guard.writer);
        let _newline_result = guard.writer.write_all(b"\n");
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
