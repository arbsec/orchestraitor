//! Catalog model and validation for `models.dev` metadata.

use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use sha2::{Digest as ShaDigest, Sha256};

use crate::error::ModelsDevError;

/// `models.dev` catalog preserving unknown fields for forward compatibility.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModelsDevCatalog {
    /// Raw JSON catalog document.
    pub raw: serde_json::Value,
    /// Hex SHA-256 digest of the raw catalog bytes.
    pub digest: String,
    /// Source that served this catalog.
    pub source: CatalogSource,
    /// Retrieval timestamp.
    #[serde(skip)]
    pub retrieved_at: SystemTime,
    /// HTTP entity tag when the live endpoint supplied one.
    pub etag: Option<String>,
}

impl ModelsDevCatalog {
    /// Parses and validates a catalog from JSON bytes.
    ///
    /// # Errors
    ///
    /// Returns an error when JSON parsing or permissive schema validation fails.
    pub fn from_bytes(
        bytes: &[u8],
        source: CatalogSource,
        etag: Option<String>,
    ) -> Result<Self, ModelsDevError> {
        let raw: serde_json::Value = serde_json::from_slice(bytes).map_err(ModelsDevError::Json)?;
        validate_catalog_shape(&raw)?;
        Ok(Self {
            raw,
            digest: sha256_hex(bytes),
            source,
            retrieved_at: SystemTime::now(),
            etag,
        })
    }

    /// Returns true when the in-memory catalog is inside the supplied TTL.
    #[must_use]
    pub fn is_fresh(&self, ttl: Duration) -> bool {
        self.retrieved_at.elapsed().is_ok_and(|age| age <= ttl)
    }

    /// Returns the auth env var names advertised for a provider id.
    #[must_use]
    pub fn provider_env_vars(&self, provider_id: &str) -> Vec<String> {
        self.raw
            .get("providers")
            .and_then(|providers| providers.get(provider_id))
            .and_then(|provider| provider.get("env"))
            .and_then(serde_json::Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(ToOwned::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Source that served a `models.dev` catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CatalogSource {
    /// Live `models.dev` endpoint.
    Live,
    /// Latest local cached snapshot.
    Cached,
    /// Bundled compile-time fallback snapshot.
    BundledFallback,
}

fn validate_catalog_shape(raw: &serde_json::Value) -> Result<(), ModelsDevError> {
    let Some(object) = raw.as_object() else {
        return Err(ModelsDevError::InvalidSchema(
            "top-level JSON value must be an object",
        ));
    };
    if !object.contains_key("providers") || !object.contains_key("models") {
        return Err(ModelsDevError::InvalidSchema(
            "top-level catalog must contain providers and models",
        ));
    }
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut rendered = String::with_capacity(64);
    for byte in digest {
        rendered.push(nibble_hex(byte >> 4));
        rendered.push(nibble_hex(byte & 0x0f));
    }
    rendered
}

fn nibble_hex(value: u8) -> char {
    match value {
        0..=9 => char::from(b'0' + value),
        10..=15 => char::from(b'a' + (value - 10)),
        _ => '?',
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_catalog_validates() -> Result<(), ModelsDevError> {
        let catalog = ModelsDevCatalog::from_bytes(
            crate::client::BUNDLED_CATALOG_BYTES,
            CatalogSource::BundledFallback,
            None,
        )?;

        assert_eq!(catalog.source, CatalogSource::BundledFallback);
        assert_eq!(catalog.digest.len(), 64);
        Ok(())
    }

    #[test]
    fn catalog_reads_provider_env_metadata() -> Result<(), ModelsDevError> {
        let bytes = br#"{"providers":{"neuralwatt":{"env":["NEURALWATT_API_KEY"]}},"models":{}}"#;
        let catalog = ModelsDevCatalog::from_bytes(bytes, CatalogSource::Cached, None)?;

        assert_eq!(
            catalog.provider_env_vars("neuralwatt"),
            vec!["NEURALWATT_API_KEY"]
        );
        Ok(())
    }
}
