//! Content digests used across the workspace.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// A SHA-256 digest, represented as a hex string.
///
/// Created by trusted components (the workspace controller, the event store,
/// Arbitraitor). Never computed inside Orchestraitor itself for security
/// purposes — only for content-addressed storage keys.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Digest(String);

impl Digest {
    /// Creates a digest from a pre-validated hex string.
    ///
    /// # Panics
    ///
    /// Panics if the string is not a 64-character lowercase hex string.
    #[must_use]
    pub fn new(hex: String) -> Self {
        debug_assert!(
            hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit()),
            "Digest must be a 64-char hex string, got: {hex}"
        );
        Self(hex)
    }

    /// Returns the hex string representation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for Digest {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit()) {
            Ok(Self(s.to_lowercase()))
        } else {
            Err(format!("invalid digest: expected 64-char hex, got {s}"))
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn digest_round_trips_through_serde() {
        let d = Digest::new("a".repeat(64));
        let json = serde_json::to_string(&d).unwrap();
        let back: Digest = serde_json::from_str(&json).unwrap();
        assert_eq!(d, back);
    }

    #[test]
    fn digest_from_str_accepts_valid() {
        let d: Digest = "abcdef0123456789".repeat(4).parse().unwrap();
        assert_eq!(d.as_str().len(), 64);
    }

    #[test]
    fn digest_from_str_rejects_invalid() {
        assert!("short".parse::<Digest>().is_err());
        assert!("g".repeat(64).parse::<Digest>().is_err());
    }
}
