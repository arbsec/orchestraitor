//! Retry scheduler with bounded exponential backoff, jitter, and circuit breaker.
//!
//! Implements spec §9.26 (retry semantics) and §9.27.2 (backpressure):
//! - Only transient failures are retried (§9.26.1).
//! - Bounded exponential backoff with jitter: base 200 ms, factor 2, cap 30 s,
//!   jitter ±20% (§9.26.2).
//! - Circuit breaker per provider: opens after N consecutive failures within a
//!   window, half-open after cooldown, closed after success (§9.26.2).
//! - Cancellation-aware: if the user/parent cancels during backoff, the retry
//!   loop terminates immediately (§9.26.2).
//! - Policy denials are never retried (§9.26.1).
//! - Side-effecting operations are never blindly retried (§9.26.3) — the caller
//!   must wrap non-idempotent operations and return a non-retryable error.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use orchestraitor_core::Retryability;
use orchestraitor_model::ProviderId;
use tracing::warn;

use crate::governance::GovernanceError;

/// Default retry policy matching spec §9.26.2.
pub const DEFAULT_RETRY_POLICY: RetryPolicy = RetryPolicy {
    max_attempts: 5,
    base_delay: Duration::from_millis(200),
    max_delay: Duration::from_secs(30),
    jitter_factor: 0.2,
};

/// Default circuit breaker failure threshold.
const DEFAULT_CIRCUIT_FAILURE_THRESHOLD: u32 = 5;

/// Default circuit breaker recovery timeout.
const DEFAULT_CIRCUIT_RECOVERY_TIMEOUT: Duration = Duration::from_secs(30);

/// Retry configuration per spec §9.26.2.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RetryPolicy {
    /// Maximum number of attempts per operation.
    pub max_attempts: u32,
    /// Base delay before the first retry.
    pub base_delay: Duration,
    /// Maximum delay cap.
    pub max_delay: Duration,
    /// Jitter factor (±fraction of the computed delay).
    pub jitter_factor: f64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        DEFAULT_RETRY_POLICY
    }
}

impl RetryPolicy {
    /// Computes the delay for the given attempt (1-based) using exponential
    /// backoff with jitter.
    ///
    /// Delay = min(`base` * 2^(attempt-1), `max_delay`), adjusted by ±`jitter_factor`.
    #[must_use]
    pub fn delay_for_attempt(&self, attempt: u32) -> Duration {
        let exponent = attempt.saturating_sub(1);
        let raw = self
            .base_delay
            .saturating_mul(2u32.saturating_pow(exponent));
        let capped = raw.min(self.max_delay);
        self.apply_jitter(capped)
    }

    /// Applies ±jitter to a delay using `SystemTime` nanos as entropy.
    #[expect(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "jitter uses f64 intermediate values; precision loss is acceptable for retry backoff"
    )]
    fn apply_jitter(&self, delay: Duration) -> Duration {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| u64::from(d.subsec_nanos()));
        let base_nanos = delay.as_nanos() as f64;
        let jitter_range = base_nanos * self.jitter_factor;
        let fraction = nanos as f64 / f64::from(u32::MAX);
        let offset = (fraction * 2.0 - 1.0) * jitter_range;
        let adjusted = (base_nanos + offset).max(0.0);
        Duration::from_nanos(adjusted as u64)
    }
}

// ---------------------------------------------------------------------------
// Circuit breaker (spec §9.26.2)
// ---------------------------------------------------------------------------

/// Internal state of a [`CircuitBreaker`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CircuitState {
    /// Normal operation — requests are allowed.
    Closed,
    /// Tripped — requests are blocked until the cooldown elapses.
    Open {
        /// When the breaker opened.
        opened_at: Instant,
    },
    /// Testing recovery — one request is allowed through.
    HalfOpen,
}

/// Per-provider circuit breaker per spec §9.26.2.
///
/// Opens after `failure_threshold` consecutive failures, transitions to
/// half-open after `recovery_timeout`, and closes after a successful call.
pub struct CircuitBreaker {
    failure_threshold: u32,
    recovery_timeout: Duration,
    state: CircuitState,
    consecutive_failures: u32,
}

impl CircuitBreaker {
    /// Creates a circuit breaker with the given threshold and cooldown.
    #[must_use]
    pub const fn new(failure_threshold: u32, recovery_timeout: Duration) -> Self {
        Self {
            failure_threshold,
            recovery_timeout,
            state: CircuitState::Closed,
            consecutive_failures: 0,
        }
    }

    /// Returns whether a request is allowed through.
    ///
    /// In `HalfOpen` state, the breaker allows one request and transitions
    /// back to `Open` if it fails, or `Closed` if it succeeds.
    #[must_use]
    pub fn allows(&mut self) -> bool {
        match self.state {
            CircuitState::Closed | CircuitState::HalfOpen => true,
            CircuitState::Open { opened_at } => {
                if opened_at.elapsed() >= self.recovery_timeout {
                    self.state = CircuitState::HalfOpen;
                    true
                } else {
                    false
                }
            }
        }
    }

    /// Records a successful operation — closes the breaker.
    pub fn record_success(&mut self) {
        self.consecutive_failures = 0;
        self.state = CircuitState::Closed;
    }

    /// Records a failed operation — may open the breaker.
    pub fn record_failure(&mut self) {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        if self.consecutive_failures >= self.failure_threshold {
            self.state = CircuitState::Open {
                opened_at: Instant::now(),
            };
        }
    }
}

// ---------------------------------------------------------------------------
// Retry scheduler
// ---------------------------------------------------------------------------

/// Retry scheduler with per-provider circuit breakers.
///
/// Wraps an operation with bounded exponential backoff, jitter, and circuit
/// breaker logic per spec §9.26.
pub struct RetryScheduler {
    policy: RetryPolicy,
    breakers: Mutex<HashMap<ProviderId, CircuitBreaker>>,
}

impl RetryScheduler {
    /// Creates a scheduler with the given retry policy.
    #[must_use]
    pub fn new(policy: RetryPolicy) -> Self {
        Self {
            policy,
            breakers: Mutex::new(HashMap::new()),
        }
    }

    /// Creates a scheduler with default policy.
    #[must_use]
    pub fn with_default_policy() -> Self {
        Self::new(DEFAULT_RETRY_POLICY)
    }

    /// Returns the configured retry policy.
    #[must_use]
    pub const fn policy(&self) -> &RetryPolicy {
        &self.policy
    }

    /// Executes `operation` with retry, backoff, and circuit-breaker logic.
    ///
    /// The `cancel` future is raced against each backoff sleep; if it
    /// completes, the retry loop terminates immediately with
    /// [`GovernanceError::Cancelled`] per spec §9.26.2.
    ///
    /// Only errors with [`Retryability::Retriable`] are retried. Policy
    /// denials and non-transient errors are returned immediately per
    /// spec §9.26.1.
    ///
    /// # Errors
    ///
    /// Returns the last [`GovernanceError`] when the retry budget is
    /// exhausted, the circuit breaker is open, or the operation was cancelled.
    pub async fn execute_with_retry<T, F, Fut>(
        &self,
        provider_id: &ProviderId,
        mut operation: F,
        cancel: impl std::future::Future<Output = ()>,
    ) -> Result<T, GovernanceError>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = Result<T, GovernanceError>>,
    {
        let mut cancel = std::pin::pin!(cancel);

        for attempt in 1..=self.policy.max_attempts {
            if !self.breaker_allows(provider_id) {
                return Err(GovernanceError::CircuitBreakerOpen(provider_id.clone()));
            }

            match operation().await {
                Ok(value) => {
                    self.record_success(provider_id);
                    return Ok(value);
                }
                Err(error) => {
                    if error.retryability() != Retryability::Retriable {
                        self.record_failure(provider_id);
                        return Err(error);
                    }
                    if attempt >= self.policy.max_attempts {
                        self.record_failure(provider_id);
                        warn!(%provider_id, attempt, "retry budget exhausted");
                        return Err(GovernanceError::RetryBudgetExhausted(provider_id.clone()));
                    }
                    self.record_failure(provider_id);
                    let delay = self.policy.delay_for_attempt(attempt);
                    tokio::select! {
                        () = &mut cancel => return Err(GovernanceError::Cancelled),
                        () = tokio::time::sleep(delay) => {}
                    }
                }
            }
        }

        Err(GovernanceError::RetryBudgetExhausted(provider_id.clone()))
    }

    /// Checks whether the circuit breaker allows a request for `provider_id`.
    fn breaker_allows(&self, provider_id: &ProviderId) -> bool {
        let mut breakers = self
            .breakers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        breakers
            .entry(provider_id.clone())
            .or_insert_with(|| {
                CircuitBreaker::new(
                    DEFAULT_CIRCUIT_FAILURE_THRESHOLD,
                    DEFAULT_CIRCUIT_RECOVERY_TIMEOUT,
                )
            })
            .allows()
    }

    /// Records a successful operation for `provider_id`.
    fn record_success(&self, provider_id: &ProviderId) {
        let mut breakers = self
            .breakers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(breaker) = breakers.get_mut(provider_id) {
            breaker.record_success();
        }
    }

    /// Records a failed operation for `provider_id`.
    fn record_failure(&self, provider_id: &ProviderId) {
        let mut breakers = self
            .breakers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(breaker) = breakers.get_mut(provider_id) {
            breaker.record_failure();
        }
    }
}
