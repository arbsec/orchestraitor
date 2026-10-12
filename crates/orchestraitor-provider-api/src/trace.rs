//! Redacting tracing layer exports for provider code.

/// Redacting tracing layer that omits sensitive fields entirely.
pub use orchestraitor_core::trace::RedactingLayer;

/// Returns whether a tracing field name is sensitive and must be omitted.
#[must_use]
pub fn is_sensitive_trace_field(field: &str) -> bool {
    orchestraitor_core::trace::is_redacted_field(field)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use tracing_subscriber::layer::SubscriberExt;

    use super::*;

    #[test]
    fn redacting_layer_omits_sensitive_fields_entirely() -> Result<(), std::string::FromUtf8Error> {
        let layer = RedactingLayer::new(Vec::new(), orchestraitor_core::trace::TracingFormat::Fmt);
        let captured = layer.clone();
        let subscriber = tracing_subscriber::registry().with(layer);

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(
                api_key = "secret",
                authorization = "Bearer token",
                user = "alice"
            );
        });

        let output = String::from_utf8(captured.bytes())?;
        assert!(output.contains("user=\"alice\""));
        assert!(!output.contains("api_key"));
        assert!(!output.contains("authorization"));
        assert!(!output.contains("secret"));
        assert!(!output.contains("Bearer token"));
        assert!(!output.contains("REDACTED"));
        Ok(())
    }
}
