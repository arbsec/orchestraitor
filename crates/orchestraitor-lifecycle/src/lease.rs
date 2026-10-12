//! Lease and TTL primitives.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Lease TTL in seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LeaseTtl(u64);

impl LeaseTtl {
    /// Creates a TTL from a duration, saturating to `u64::MAX` seconds.
    #[must_use]
    pub const fn from_duration(duration: Duration) -> Self {
        Self(duration.as_secs())
    }

    /// Default one-hour lease from spec §9.24.2.
    #[must_use]
    pub const fn default_one_hour() -> Self {
        Self(60 * 60)
    }

    /// Returns TTL seconds.
    #[must_use]
    pub const fn as_secs(self) -> u64 {
        self.0
    }
}

impl Default for LeaseTtl {
    fn default() -> Self {
        Self::default_one_hour()
    }
}

/// Durable task lease. Expiry produces `orphaned`, never direct `failed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lease {
    /// Unix timestamp seconds when the lease was acquired.
    pub acquired_at_unix_secs: u64,
    /// Lease TTL.
    pub ttl: LeaseTtl,
}

impl Lease {
    /// Creates a lease using the default one-hour TTL.
    #[must_use]
    pub const fn new(acquired_at_unix_secs: u64) -> Self {
        Self {
            acquired_at_unix_secs,
            ttl: LeaseTtl::default_one_hour(),
        }
    }

    /// Creates a lease with a caller-selected TTL.
    #[must_use]
    pub const fn with_ttl(acquired_at_unix_secs: u64, ttl: LeaseTtl) -> Self {
        Self {
            acquired_at_unix_secs,
            ttl,
        }
    }

    /// Returns the expiry timestamp in Unix seconds.
    #[must_use]
    pub const fn expires_at_unix_secs(self) -> u64 {
        self.acquired_at_unix_secs
            .saturating_add(self.ttl.as_secs())
    }

    /// Returns whether the lease is expired at `now_unix_secs`.
    #[must_use]
    pub const fn is_expired_at(self, now_unix_secs: u64) -> bool {
        now_unix_secs >= self.expires_at_unix_secs()
    }
}
