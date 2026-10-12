//! Error type for the in-house `models.dev` client.

use std::fmt;

/// `models.dev` client errors.
#[derive(Debug)]
pub enum ModelsDevError {
    /// HTTP client construction failed.
    HttpClient(reqwest::Error),
    /// HTTP fetch failed.
    Fetch(reqwest::Error),
    /// Catalog exceeded the configured byte limit.
    TooLarge(u64),
    /// Catalog response was not JSON.
    InvalidContentType,
    /// Catalog JSON parsing or serialization failed.
    Json(serde_json::Error),
    /// Catalog failed permissive schema validation.
    InvalidSchema(&'static str),
    /// Cache I/O failed.
    CacheIo(std::io::Error),
    /// A 304 response arrived before any memory cache existed.
    NotModifiedWithoutCache,
    /// Download size could not fit into u64.
    SizeOverflow,
    /// Live and fallback paths both failed.
    FallbackFailed {
        /// Original live fetch error.
        live: Box<ModelsDevError>,
        /// Fallback validation error.
        fallback: Box<ModelsDevError>,
    },
}

impl fmt::Display for ModelsDevError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HttpClient(_) => {
                formatter.write_str("models.dev HTTP client construction failed")
            }
            Self::Fetch(_) => formatter.write_str("models.dev catalog fetch failed"),
            Self::TooLarge(size) => {
                write!(formatter, "models.dev catalog exceeds byte limit: {size}")
            }
            Self::InvalidContentType => {
                formatter.write_str("models.dev catalog response is not JSON")
            }
            Self::Json(_) => formatter.write_str("models.dev catalog JSON is invalid"),
            Self::InvalidSchema(reason) => {
                write!(formatter, "models.dev catalog schema is invalid: {reason}")
            }
            Self::CacheIo(_) => formatter.write_str("models.dev catalog cache I/O failed"),
            Self::NotModifiedWithoutCache => {
                formatter.write_str("models.dev returned 304 without cached catalog")
            }
            Self::SizeOverflow => formatter.write_str("models.dev catalog byte length overflowed"),
            Self::FallbackFailed {
                live: _,
                fallback: _,
            } => formatter.write_str("models.dev live and fallback catalogs failed"),
        }
    }
}

impl std::error::Error for ModelsDevError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::HttpClient(error) | Self::Fetch(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::CacheIo(error) => Some(error),
            Self::FallbackFailed { live, fallback: _ } => Some(live),
            Self::TooLarge(_)
            | Self::InvalidContentType
            | Self::InvalidSchema(_)
            | Self::NotModifiedWithoutCache
            | Self::SizeOverflow => None,
        }
    }
}
