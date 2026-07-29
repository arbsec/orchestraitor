//! Tracing setup with field-name redaction.

use std::fmt::Write as FmtWrite;
use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use tracing::{Event, Subscriber, field::Visit};
use tracing_subscriber::{EnvFilter, Layer, Registry, layer::Context, layer::SubscriberExt};

use crate::error::{OrchestraitorError, TracingError};

/// Output format for tracing events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TracingFormat {
    /// Human-readable key-value format.
    Fmt,
    /// JSON Lines format.
    Json,
}

/// Options for tracing initialization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TracingOptions {
    /// Environment filter directive, for example `info,orchestraitor=debug`.
    pub env_filter: String,
    /// Event output format.
    pub format: TracingFormat,
}

impl Default for TracingOptions {
    fn default() -> Self {
        Self {
            env_filter: "info".to_string(),
            format: TracingFormat::Fmt,
        }
    }
}

/// Initializes global tracing with env-filtering and redacted fields.
pub struct TracingInit;

impl TracingInit {
    /// Initializes the global tracing subscriber.
    ///
    /// # Errors
    ///
    /// Returns an error when the environment filter is invalid or tracing is already initialized.
    pub fn try_init(options: &TracingOptions) -> Result<(), OrchestraitorError> {
        let filter = EnvFilter::try_new(&options.env_filter)
            .map_err(|error| TracingError::EnvFilter(Box::new(error)))?;
        let layer = RedactingLayer::new(io::stderr(), options.format);
        let subscriber = Registry::default().with(filter).with(layer);
        tracing::subscriber::set_global_default(subscriber)
            .map_err(|error| TracingError::AlreadyInitialized(Box::new(error)))?;
        Ok(())
    }
}

/// Tracing layer that omits sensitive fields by field name.
#[derive(Debug)]
pub struct RedactingLayer<W> {
    writer: Arc<Mutex<W>>,
    format: TracingFormat,
}

impl<W> Clone for RedactingLayer<W> {
    fn clone(&self) -> Self {
        Self {
            writer: Arc::clone(&self.writer),
            format: self.format,
        }
    }
}

impl<W> RedactingLayer<W>
where
    W: Write,
{
    /// Creates a redacting layer writing to the supplied sink.
    #[must_use]
    pub fn new(writer: W, format: TracingFormat) -> Self {
        Self {
            writer: Arc::new(Mutex::new(writer)),
            format,
        }
    }
}

impl RedactingLayer<Vec<u8>> {
    /// Returns captured bytes for test sinks.
    #[must_use]
    pub fn bytes(&self) -> Vec<u8> {
        match self.writer.lock() {
            Ok(writer) => writer.clone(),
            Err(_) => Vec::new(),
        }
    }
}

impl<S, W> Layer<S> for RedactingLayer<W>
where
    S: Subscriber,
    W: Write + Send + 'static,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut visitor = RedactingVisitor::default();
        event.record(&mut visitor);
        let mut line = String::new();
        match self.format {
            TracingFormat::Fmt => format_fmt_event(event, &visitor, &mut line),
            TracingFormat::Json => format_json_event(event, &visitor, &mut line),
        }
        if let Ok(mut writer) = self.writer.lock() {
            let _write_result = writeln!(writer, "{line}");
        }
    }
}

/// Returns whether a tracing field name must be redacted.
#[must_use]
pub fn is_redacted_field(field: &str) -> bool {
    let normalized = field.to_ascii_lowercase().replace('-', "_");
    normalized == "api_key"
        || normalized == "authorization"
        || normalized == "bearer"
        || normalized == "x_api_key"
        || normalized == "x_goog_api_key"
        || normalized.ends_with("_key")
        || normalized.ends_with("_secret")
        || normalized.ends_with("_token")
}

#[derive(Debug, Default)]
struct RedactingVisitor {
    fields: Vec<(String, String)>,
}

impl Visit for RedactingVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        let name = field.name();
        if !is_redacted_field(name) {
            self.fields.push((name.to_string(), format!("{value:?}")));
        }
    }
}

fn format_fmt_event(event: &Event<'_>, visitor: &RedactingVisitor, line: &mut String) {
    let _format_result = write!(
        line,
        "{} {}",
        event.metadata().level(),
        event.metadata().target()
    );
    for (key, value) in &visitor.fields {
        let _format_result = write!(line, " {key}={value}");
    }
}

fn format_json_event(event: &Event<'_>, visitor: &RedactingVisitor, line: &mut String) {
    line.push('{');
    let _format_result = write!(
        line,
        "\"level\":\"{}\",\"target\":\"{}\"",
        event.metadata().level(),
        event.metadata().target()
    );
    for (key, value) in &visitor.fields {
        let _format_result = write!(line, ",\"{}\":\"{}\"", escape_json(key), escape_json(value));
    }
    line.push('}');
}

fn escape_json(input: &str) -> String {
    input.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacting_layer_omits_sensitive_fields() -> Result<(), std::string::FromUtf8Error> {
        let layer = RedactingLayer::new(Vec::new(), TracingFormat::Fmt);
        let captured = layer.clone();
        let subscriber = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(
                api_key = "super-secret",
                user = "alice",
                authorization = "Bearer token"
            );
        });
        let output = String::from_utf8(captured.bytes())?;
        assert!(output.contains("user=\"alice\""));
        assert!(!output.contains("api_key"));
        assert!(!output.contains("authorization"));
        assert!(!output.contains("super-secret"));
        assert!(!output.contains("Bearer token"));
        assert!(!output.contains("[REDACTED]"));
        Ok(())
    }

    #[test]
    fn redaction_field_matcher_covers_required_names() {
        for field in [
            "provider_key",
            "client_secret",
            "api_key",
            "authorization",
            "session_token",
            "bearer",
            "x-api-key",
            "x-goog-api-key",
        ] {
            assert!(is_redacted_field(field));
        }
        assert!(!is_redacted_field("provider"));
    }
}
