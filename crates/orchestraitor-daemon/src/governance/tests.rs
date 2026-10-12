//! Tests for data-governance routing and retry scheduling.

#![allow(clippy::unwrap_used)]

use std::sync::Mutex;
use std::time::Duration;

use orchestraitor_core::Retryability;
use orchestraitor_model::ProviderId;

use super::retry::{RetryPolicy, RetryScheduler};
use super::{
    ArbitraitorEnforcer, CircuitBreaker, ClassifiedContent, DataClassification, DataEgressPreview,
    DataReleaseEnforcer, GovernanceError, GovernanceEventSink, GovernanceRouter,
    ProviderDataGovernance, ReleaseVerdict, RoutingDecision,
};

// ---------------------------------------------------------------------------
// Test doubles
// ---------------------------------------------------------------------------

/// Fake enforcer that records all calls and returns a configured verdict.
struct FakeEnforcer {
    calls: Mutex<Vec<(ClassifiedContent, ProviderId)>>,
    verdict: ReleaseVerdict,
}

impl FakeEnforcer {
    fn new(verdict: ReleaseVerdict) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            verdict,
        }
    }

    fn call_count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }
}

impl DataReleaseEnforcer for FakeEnforcer {
    fn enforce_data_release(
        &self,
        content: &ClassifiedContent,
        destination: &ProviderId,
    ) -> Result<ReleaseVerdict, GovernanceError> {
        self.calls
            .lock()
            .unwrap()
            .push((content.clone(), destination.clone()));
        Ok(self.verdict.clone())
    }
}

/// Fake event sink that records all emitted previews.
struct FakeEventSink {
    previews: Mutex<Vec<DataEgressPreview>>,
}

impl FakeEventSink {
    fn new() -> Self {
        Self {
            previews: Mutex::new(Vec::new()),
        }
    }

    fn preview_count(&self) -> usize {
        self.previews.lock().unwrap().len()
    }

    fn last_preview(&self) -> DataEgressPreview {
        self.previews.lock().unwrap().last().cloned().unwrap()
    }
}

impl GovernanceEventSink for FakeEventSink {
    fn record_data_egress_preview(&self, preview: DataEgressPreview) {
        self.previews.lock().unwrap().push(preview);
    }
}

fn make_router(verdict: ReleaseVerdict) -> (GovernanceRouter, std::sync::Arc<FakeEnforcer>) {
    let enforcer = std::sync::Arc::new(FakeEnforcer::new(verdict));
    let sink = std::sync::Arc::new(FakeEventSink::new());
    let router = GovernanceRouter::new(
        std::sync::Arc::clone(&enforcer) as std::sync::Arc<dyn DataReleaseEnforcer>,
        sink as std::sync::Arc<dyn GovernanceEventSink>,
    );
    (router, enforcer)
}

fn test_provider() -> ProviderId {
    ProviderId::from_string("neuralwatt".to_string())
}

fn content(classification: DataClassification, paths: &[&str]) -> ClassifiedContent {
    ClassifiedContent {
        file_paths: paths.iter().map(std::string::ToString::to_string).collect(),
        classification,
    }
}

fn governance(allowed: &[DataClassification], prohibited: &[&str]) -> ProviderDataGovernance {
    ProviderDataGovernance {
        allowed_classes: allowed.to_vec(),
        prohibited_patterns: prohibited
            .iter()
            .map(std::string::ToString::to_string)
            .collect(),
    }
}

// ---------------------------------------------------------------------------
// Routing tests (spec §9.28.2)
// ---------------------------------------------------------------------------

#[test]
fn restricted_content_never_reaches_remote_provider() {
    // Given: restricted content and a provider that allows internal+confidential.
    let (router, enforcer) = make_router(ReleaseVerdict::Deny {
        reason: "restricted".to_string(),
    });
    let provider = test_provider();
    let gov = governance(
        &[
            DataClassification::Internal,
            DataClassification::Confidential,
        ],
        &[],
    );
    let content = content(DataClassification::Restricted, &["src/secrets/api_key.pem"]);

    // When: routing restricted content.
    let decision = router.route(&content, &provider, &gov).unwrap();

    // Then: the content is blocked and Arbitraitor was called for enforcement.
    assert_eq!(decision, RoutingDecision::Block);
    assert_eq!(
        enforcer.call_count(),
        1,
        "Arbitraitor enforcement must be called for restricted content"
    );
}

#[test]
fn internal_content_allowed_to_approved_provider() {
    // Given: internal content and a provider that allows internal.
    let (router, enforcer) = make_router(ReleaseVerdict::Permit);
    let provider = test_provider();
    let gov = governance(&[DataClassification::Internal], &[]);
    let content = content(DataClassification::Internal, &["src/main.rs"]);

    // When: routing internal content.
    let decision = router.route(&content, &provider, &gov).unwrap();

    // Then: the content is allowed and no enforcement call was needed.
    assert_eq!(decision, RoutingDecision::Allow);
    assert_eq!(
        enforcer.call_count(),
        0,
        "no enforcement call for allowed content"
    );
}

#[test]
fn confidential_content_blocked_from_unapproved_provider() {
    // Given: confidential content and a provider that only allows internal.
    let (router, enforcer) = make_router(ReleaseVerdict::Deny {
        reason: "not approved".to_string(),
    });
    let provider = test_provider();
    let gov = governance(&[DataClassification::Internal], &[]);
    let content = content(DataClassification::Confidential, &["src/private.rs"]);

    // When: routing confidential content to an unapproved provider.
    let decision = router.route(&content, &provider, &gov).unwrap();

    // Then: the content is blocked.
    assert_eq!(decision, RoutingDecision::Block);
    assert_eq!(enforcer.call_count(), 1);
}

#[test]
fn prohibited_path_blocks_even_if_class_allowed() {
    // Given: internal content with a path matching a prohibited pattern.
    let (router, enforcer) = make_router(ReleaseVerdict::Deny {
        reason: "prohibited path".to_string(),
    });
    let provider = test_provider();
    let gov = governance(&[DataClassification::Internal], &["**/.aws/**"]);
    let content = content(DataClassification::Internal, &["config/.aws/credentials"]);

    // When: routing content with a prohibited path.
    let decision = router.route(&content, &provider, &gov).unwrap();

    // Then: the content is blocked.
    assert_eq!(decision, RoutingDecision::Block);
    assert_eq!(enforcer.call_count(), 1);
}

// ---------------------------------------------------------------------------
// Data egress preview tests (spec §9.28.2)
// ---------------------------------------------------------------------------

#[test]
fn data_egress_preview_emitted_when_content_allowed() {
    // Given: a router with a recording event sink.
    let enforcer = std::sync::Arc::new(FakeEnforcer::new(ReleaseVerdict::Permit));
    let sink = std::sync::Arc::new(FakeEventSink::new());
    let router = GovernanceRouter::new(
        std::sync::Arc::clone(&enforcer) as std::sync::Arc<dyn DataReleaseEnforcer>,
        std::sync::Arc::clone(&sink) as std::sync::Arc<dyn GovernanceEventSink>,
    );
    let provider = test_provider();
    let gov = governance(&[DataClassification::Internal], &[]);
    let content = content(DataClassification::Internal, &["src/main.rs", "src/lib.rs"]);

    // When: routing allowed content.
    let decision = router.route(&content, &provider, &gov).unwrap();

    // Then: a data_egress.preview event was emitted with the correct fields.
    assert_eq!(decision, RoutingDecision::Allow);
    assert_eq!(sink.preview_count(), 1);
    let preview = sink.last_preview();
    assert_eq!(preview.file_paths, vec!["src/main.rs", "src/lib.rs"]);
    assert_eq!(preview.classification, DataClassification::Internal);
    assert_eq!(preview.destination, provider);
}

#[test]
fn data_egress_preview_not_emitted_when_content_blocked() {
    // Given: a router that blocks restricted content.
    let enforcer = std::sync::Arc::new(FakeEnforcer::new(ReleaseVerdict::Deny {
        reason: "restricted".to_string(),
    }));
    let sink = std::sync::Arc::new(FakeEventSink::new());
    let router = GovernanceRouter::new(
        std::sync::Arc::clone(&enforcer) as std::sync::Arc<dyn DataReleaseEnforcer>,
        std::sync::Arc::clone(&sink) as std::sync::Arc<dyn GovernanceEventSink>,
    );
    let provider = test_provider();
    let gov = governance(&[DataClassification::Internal], &[]);
    let content = content(DataClassification::Restricted, &["secrets/key.pem"]);

    // When: routing blocked content.
    let decision = router.route(&content, &provider, &gov).unwrap();

    // Then: no preview event was emitted (content is not leaving the machine).
    assert_eq!(decision, RoutingDecision::Block);
    assert_eq!(sink.preview_count(), 0);
}

// ---------------------------------------------------------------------------
// Retry classification tests (spec §9.26.1)
// ---------------------------------------------------------------------------

#[test]
fn policy_denials_are_not_retryable() {
    // Given: a policy denial error.
    let error = GovernanceError::PolicyDenial {
        destination: test_provider(),
        reason: "blocked by policy".to_string(),
    };

    // Then: it is classified as not retryable.
    assert_eq!(error.retryability(), Retryability::NotRetriable);
}

#[test]
fn restricted_content_blocked_is_not_retryable() {
    let error = GovernanceError::RestrictedContentBlocked {
        destination: test_provider(),
    };
    assert_eq!(error.retryability(), Retryability::NotRetriable);
}

#[test]
fn transient_transport_errors_are_retryable() {
    let error = GovernanceError::TransientTransport {
        provider: test_provider(),
        reason: "connection reset".to_string(),
    };
    assert_eq!(error.retryability(), Retryability::Retriable);
}

// ---------------------------------------------------------------------------
// Retry budget tests (spec §9.26.2)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn retry_budget_enforced_after_max_attempts() {
    // Given: a scheduler with max_attempts=3 and a fast policy.
    let policy = RetryPolicy {
        max_attempts: 3,
        base_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(1),
        jitter_factor: 0.0,
    };
    let scheduler = RetryScheduler::new(policy);
    let provider = test_provider();
    let attempts = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));

    // When: an operation that always fails with a transient error.
    let attempts_clone = std::sync::Arc::clone(&attempts);
    let result = scheduler
        .execute_with_retry(
            &provider,
            move || {
                let attempts = std::sync::Arc::clone(&attempts_clone);
                async move {
                    attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Err::<(), GovernanceError>(GovernanceError::TransientTransport {
                        provider: ProviderId::from_string("neuralwatt".to_string()),
                        reason: "timeout".to_string(),
                    })
                }
            },
            std::future::pending::<()>(),
        )
        .await;

    // Then: the operation was attempted exactly 3 times and the budget was exhausted.
    assert!(matches!(
        result,
        Err(GovernanceError::RetryBudgetExhausted(_))
    ));
    assert_eq!(
        attempts.load(std::sync::atomic::Ordering::SeqCst),
        3,
        "operation must be attempted exactly max_attempts times"
    );
}

#[tokio::test]
async fn non_retryable_error_returns_immediately() {
    // Given: a scheduler and an operation that fails with a policy denial.
    let scheduler = RetryScheduler::with_default_policy();
    let provider = test_provider();
    let attempts = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));

    // When: the operation returns a non-retryable error.
    let attempts_clone = std::sync::Arc::clone(&attempts);
    let result = scheduler
        .execute_with_retry(
            &provider,
            move || {
                let attempts = std::sync::Arc::clone(&attempts_clone);
                async move {
                    attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Err::<(), GovernanceError>(GovernanceError::PolicyDenial {
                        destination: ProviderId::from_string("neuralwatt".to_string()),
                        reason: "denied".to_string(),
                    })
                }
            },
            std::future::pending::<()>(),
        )
        .await;

    // Then: the operation was attempted only once.
    assert!(matches!(result, Err(GovernanceError::PolicyDenial { .. })));
    assert_eq!(
        attempts.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "non-retryable errors must not be retried"
    );
}

#[tokio::test]
async fn cancellation_terminates_retry_loop() {
    // Given: a scheduler with a long backoff.
    let policy = RetryPolicy {
        max_attempts: 10,
        base_delay: Duration::from_mins(1),
        max_delay: Duration::from_mins(1),
        jitter_factor: 0.0,
    };
    let scheduler = RetryScheduler::new(policy);
    let provider = test_provider();

    // When: the operation fails and cancellation fires after 10ms.
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        scheduler.execute_with_retry(
            &provider,
            || async {
                Err::<(), GovernanceError>(GovernanceError::TransientTransport {
                    provider: ProviderId::from_string("neuralwatt".to_string()),
                    reason: "timeout".to_string(),
                })
            },
            tokio::time::sleep(Duration::from_millis(10)),
        ),
    )
    .await;

    // Then: the retry loop was terminated by cancellation (not by timeout).
    assert!(
        result.is_ok(),
        "cancellation should terminate before the 2s timeout"
    );
    let inner = result.unwrap();
    assert!(matches!(inner, Err(GovernanceError::Cancelled)));
}

// ---------------------------------------------------------------------------
// Circuit breaker tests (spec §9.26.2)
// ---------------------------------------------------------------------------

#[test]
fn circuit_breaker_opens_after_threshold_failures() {
    // Given: a breaker with threshold=3 and a long cooldown.
    let mut breaker = CircuitBreaker::new(3, Duration::from_mins(1));

    // When: recording 3 consecutive failures.
    assert!(breaker.allows());
    breaker.record_failure();
    assert!(breaker.allows());
    breaker.record_failure();
    assert!(breaker.allows());
    breaker.record_failure();

    // Then: the breaker is now open and blocks requests.
    assert!(!breaker.allows(), "breaker should be open after 3 failures");
}

#[test]
fn circuit_breaker_closes_after_success() {
    // Given: a breaker with a long cooldown that is open.
    let mut breaker = CircuitBreaker::new(1, Duration::from_mins(1));
    breaker.record_failure();
    assert!(!breaker.allows(), "breaker should be open immediately");

    // When: recording a success.
    breaker.record_success();

    // Then: the breaker is closed and allows requests.
    assert!(breaker.allows());
}

#[test]
fn circuit_breaker_half_open_after_cooldown() {
    // Given: a breaker with a short cooldown.
    let mut breaker = CircuitBreaker::new(1, Duration::from_millis(1));
    breaker.record_failure();
    assert!(!breaker.allows(), "breaker should be open immediately");

    // When: waiting for the cooldown to elapse.
    std::thread::sleep(Duration::from_millis(5));

    // Then: the breaker transitions to half-open and allows a request.
    assert!(breaker.allows(), "breaker should allow after cooldown");
}

// ---------------------------------------------------------------------------
// Glob matching tests
// ---------------------------------------------------------------------------

#[test]
fn glob_matches_double_star_patterns() {
    assert!(super::matches_glob("**/env.local", "config/env.local"));
    assert!(super::matches_glob("**/.aws/**", "home/.aws/credentials"));
    assert!(super::matches_glob(
        "**/secrets/**",
        "src/secrets/api_key.rs"
    ));
    assert!(!super::matches_glob("**/env.local", "config/env.prod"));
    assert!(!super::matches_glob("**/.aws/**", "home/.config/config"));
}

#[test]
fn glob_exact_match_without_double_star() {
    assert!(super::matches_glob("exact/path", "exact/path"));
    assert!(!super::matches_glob("exact/path", "other/path"));
}

// ---------------------------------------------------------------------------
// Arbitraitor enforcer tests (spec §9.28.4)
// ---------------------------------------------------------------------------

#[test]
fn arbitraitor_enforcer_delegates_to_arbitraitor_client() {
    // Given: an enforcer backed by a default ArbitraitorClient.
    use orchestraitor_arbitraitor_client::ArbitraitorClient;
    let enforcer = ArbitraitorEnforcer::new(ArbitraitorClient::default());
    let content = content(DataClassification::Restricted, &["secrets/key.pem"]);
    let provider = test_provider();

    // When: enforcing data-release for restricted content.
    // Then: the call to Arbitraitor succeeds (does not panic or return Err
    // from Orchestraitor-local code). The verdict is determined by
    // Arbitraitor's policy engine, not by Orchestraitor.
    let result = enforcer.enforce_data_release(&content, &provider);
    assert!(
        result.is_ok(),
        "Arbitraitor enforcement call should succeed, got: {result:?}"
    );
}
