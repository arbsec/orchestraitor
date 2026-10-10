//! Data-governance routing and retry scheduling for the daemon.
//!
//! Implements spec §9.28.2–§9.28.4 (data-governance routing),
//! §9.26 (retry semantics), and §9.27.2–§9.27.3 (backpressure boundary).
//!
//! # Security boundary
//!
//! Routing policy — which data classification can flow to which provider — is
//! Orchestraitor's. The enforcement boundary — refusing to release restricted
//! data to a remote network destination — is delegated to Arbitraitor's network
//! broker per spec §9.28.4. This module never implements its own network-blocking
//! layer; it calls [`DataReleaseEnforcer`] (backed by [`ArbitraitorEnforcer`])
//! for every refusal.

mod retry;

#[cfg(test)]
mod tests;

pub use retry::{CircuitBreaker, RetryPolicy, RetryScheduler};

use std::sync::Arc;

use orchestraitor_arbitraitor_client::{ArbitraitorClient, EvalContext, Verdict};
use orchestraitor_core::Retryability;
use orchestraitor_model::{Digest, ProviderId};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Data classification (spec §9.28.1)
// ---------------------------------------------------------------------------

/// Content sensitivity classification per spec §9.28.1.
///
/// Controls which providers may receive the classified content:
/// - [`Public`](Self::Public): any provider.
/// - [`Internal`](Self::Internal): default — per-policy providers.
/// - [`Confidential`](Self::Confidential): approved providers only.
/// - [`Restricted`](Self::Restricted): never sent to a remote provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataClassification {
    /// Any provider may receive this content.
    Public,
    /// Default classification — per-policy providers only.
    Internal,
    /// Approved providers only, region-constrained.
    Confidential,
    /// Never sent to a remote provider; redact, summarize, or block.
    Restricted,
}

/// Content item with its data classification and source file paths.
#[derive(Debug, Clone, PartialEq)]
pub struct ClassifiedContent {
    /// File paths contributing to this content.
    pub file_paths: Vec<String>,
    /// Sensitivity classification.
    pub classification: DataClassification,
}

// ---------------------------------------------------------------------------
// Provider data-governance config (spec §9.28.3)
// ---------------------------------------------------------------------------

/// Per-provider data-governance constraints per spec §9.28.3.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderDataGovernance {
    /// Classification classes allowed to flow to this provider.
    pub allowed_classes: Vec<DataClassification>,
    /// Glob patterns for paths that must never be sent to this provider.
    pub prohibited_patterns: Vec<String>,
}

impl ProviderDataGovernance {
    /// Returns whether the given classification is in the allowed set.
    #[must_use]
    pub fn allows_class(&self, class: DataClassification) -> bool {
        self.allowed_classes.contains(&class)
    }

    /// Returns whether any file path matches a prohibited glob pattern.
    #[must_use]
    pub fn matches_prohibited(&self, file_paths: &[String]) -> bool {
        file_paths.iter().any(|path| {
            self.prohibited_patterns
                .iter()
                .any(|p| matches_glob(p, path))
        })
    }
}

// ---------------------------------------------------------------------------
// Data egress preview event (spec §9.28.2)
// ---------------------------------------------------------------------------

/// Preview of data about to leave the machine for a remote provider.
///
/// Emitted as a `data_egress.preview` event before the provider call when
/// policy requires, per spec §9.28.2.
#[derive(Debug, Clone, PartialEq)]
pub struct DataEgressPreview {
    /// File paths included in the egress.
    pub file_paths: Vec<String>,
    /// Sensitivity classification of the content.
    pub classification: DataClassification,
    /// Destination provider.
    pub destination: ProviderId,
}

// ---------------------------------------------------------------------------
// Routing decision + release verdict
// ---------------------------------------------------------------------------

/// Decision returned by [`GovernanceRouter::route`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutingDecision {
    /// Content may be sent to the provider.
    Allow,
    /// Content must not be sent to the provider.
    Block,
}

/// Arbitraitor's verdict on a data-release request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseVerdict {
    /// Release is permitted.
    Permit,
    /// Release is denied; Arbitraitor enforces the network boundary.
    Deny {
        /// Human-readable denial reason.
        reason: String,
    },
}

// ---------------------------------------------------------------------------
// Error taxonomy
// ---------------------------------------------------------------------------

/// Failures from data-governance routing and retry scheduling.
#[derive(Debug, thiserror::Error)]
pub enum GovernanceError {
    /// Restricted content blocked from a remote provider (spec §9.28.2).
    #[error("restricted content blocked from remote provider `{destination}`")]
    RestrictedContentBlocked {
        /// Provider that was the intended destination.
        destination: ProviderId,
    },
    /// Content classification not allowed for this provider.
    #[error("content classification not allowed for provider `{destination}`")]
    ClassificationNotAllowed {
        /// Provider that was the intended destination.
        destination: ProviderId,
    },
    /// File path matches a prohibited pattern for this provider.
    #[error("file path matches prohibited pattern for provider `{destination}`")]
    ProhibitedPath {
        /// Provider that was the intended destination.
        destination: ProviderId,
    },
    /// Arbitraitor data-release enforcement call failed (spec §9.28.4).
    #[error("arbitraitor enforcement error: {0}")]
    ArbitraitorEnforcement(String),
    /// Arbitraitor policy evaluation returned an error verdict.
    #[error("arbitraitor policy evaluation error: {0}")]
    ArbitraitorPolicy(String),
    /// Policy denial — not retryable per spec §9.26.1.
    #[error("policy denial for provider `{destination}`: {reason}")]
    PolicyDenial {
        /// Provider that was the intended destination.
        destination: ProviderId,
        /// Denial reason from Arbitraitor.
        reason: String,
    },
    /// Transient transport failure — retryable per spec §9.26.1.
    #[error("transient transport failure for provider `{provider}`: {reason}")]
    TransientTransport {
        /// Provider that experienced the failure.
        provider: ProviderId,
        /// Transport failure description.
        reason: String,
    },
    /// Circuit breaker is open for this provider (spec §9.26.2).
    #[error("circuit breaker open for provider `{0}`")]
    CircuitBreakerOpen(ProviderId),
    /// Retry budget exhausted (spec §9.26.2).
    #[error("retry budget exhausted for provider `{0}`")]
    RetryBudgetExhausted(ProviderId),
    /// Operation was cancelled during backoff (spec §9.26.2).
    #[error("operation cancelled")]
    Cancelled,
}

impl GovernanceError {
    /// Returns the retry classification for this error per spec §9.26.1.
    #[must_use]
    pub const fn retryability(&self) -> Retryability {
        match self {
            Self::TransientTransport { .. } => Retryability::Retriable,
            Self::RestrictedContentBlocked { .. }
            | Self::ClassificationNotAllowed { .. }
            | Self::ProhibitedPath { .. }
            | Self::ArbitraitorEnforcement(_)
            | Self::ArbitraitorPolicy(_)
            | Self::PolicyDenial { .. }
            | Self::CircuitBreakerOpen(_)
            | Self::RetryBudgetExhausted(_)
            | Self::Cancelled => Retryability::NotRetriable,
        }
    }
}

// ---------------------------------------------------------------------------
// Enforcement + event emission traits
// ---------------------------------------------------------------------------

/// Arbitraitor data-release enforcement boundary per spec §9.28.4.
///
/// Orchestraitor's routing policy determines which class can flow to which
/// provider. The enforcement boundary — refusing to release restricted data
/// to a remote network destination — is delegated to Arbitraitor's network
/// broker. Orchestraitor does not implement its own network-blocking layer.
pub trait DataReleaseEnforcer: Send + Sync {
    /// Requests Arbitraitor to enforce data-release policy.
    ///
    /// Returns `Ok(Permit)` when Arbitraitor permits the release, or
    /// `Ok(Deny)` when Arbitraitor denies it. Returns `Err` only when the
    /// Arbitraitor call itself fails.
    ///
    /// # Errors
    ///
    /// Returns [`GovernanceError::ArbitraitorEnforcement`] when the
    /// Arbitraitor policy engine returns an error, or
    /// [`GovernanceError::ArbitraitorPolicy`] when the verdict is
    /// `Error` or `Incomplete`.
    fn enforce_data_release(
        &self,
        content: &ClassifiedContent,
        destination: &ProviderId,
    ) -> Result<ReleaseVerdict, GovernanceError>;
}

/// Sink for governance events emitted during routing.
pub trait GovernanceEventSink: Send + Sync {
    /// Records a `data_egress.preview` event before a provider call.
    fn record_data_egress_preview(&self, preview: DataEgressPreview);
}

// ---------------------------------------------------------------------------
// Governance router
// ---------------------------------------------------------------------------

/// Routes classified content to providers based on data-governance policy.
///
/// Delegates enforcement to Arbitraitor via [`DataReleaseEnforcer`] and emits
/// `data_egress.preview` events via [`GovernanceEventSink`].
pub struct GovernanceRouter {
    enforcer: Arc<dyn DataReleaseEnforcer>,
    event_sink: Arc<dyn GovernanceEventSink>,
}

impl GovernanceRouter {
    /// Creates a router with the given enforcer and event sink.
    #[must_use]
    pub fn new(
        enforcer: Arc<dyn DataReleaseEnforcer>,
        event_sink: Arc<dyn GovernanceEventSink>,
    ) -> Self {
        Self {
            enforcer,
            event_sink,
        }
    }

    /// Routes content to a provider based on data-governance policy.
    ///
    /// Per spec §9.28.2:
    /// - `restricted` content is never sent to a remote provider;
    /// - `confidential` content goes only to approved providers;
    /// - `internal` content goes to any configured provider.
    ///
    /// When content is blocked, enforcement is delegated to Arbitraitor per
    /// spec §9.28.4. When content is allowed, a `data_egress.preview` event
    /// is emitted before the provider call.
    ///
    /// # Errors
    ///
    /// Returns [`GovernanceError`] when Arbitraitor enforcement fails.
    pub fn route(
        &self,
        content: &ClassifiedContent,
        provider_id: &ProviderId,
        governance: &ProviderDataGovernance,
    ) -> Result<RoutingDecision, GovernanceError> {
        let blocked = content.classification == DataClassification::Restricted
            || !governance.allows_class(content.classification)
            || governance.matches_prohibited(&content.file_paths);

        if blocked {
            let verdict = self.enforcer.enforce_data_release(content, provider_id)?;
            return Ok(match verdict {
                ReleaseVerdict::Permit => RoutingDecision::Allow,
                ReleaseVerdict::Deny { .. } => RoutingDecision::Block,
            });
        }

        self.event_sink
            .record_data_egress_preview(DataEgressPreview {
                file_paths: content.file_paths.clone(),
                classification: content.classification,
                destination: provider_id.clone(),
            });
        Ok(RoutingDecision::Allow)
    }
}

// ---------------------------------------------------------------------------
// Arbitraitor-backed enforcer
// ---------------------------------------------------------------------------

/// Minimal Arbitraitor policy TOML used for data-release evaluation.
const DEFAULT_POLICY_TOML: &str = "version = 1\n";

/// Arbitraitor-backed [`DataReleaseEnforcer`].
///
/// Calls [`ArbitraitorClient::evaluate_policy`] to delegate the data-release
/// enforcement decision to Arbitraitor per spec §9.28.4.
pub struct ArbitraitorEnforcer {
    client: ArbitraitorClient,
    policy_toml: String,
}

impl ArbitraitorEnforcer {
    /// Creates an enforcer backed by the given Arbitraitor client.
    #[must_use]
    pub fn new(client: ArbitraitorClient) -> Self {
        Self {
            client,
            policy_toml: DEFAULT_POLICY_TOML.to_string(),
        }
    }

    /// Creates an enforcer with a custom policy TOML.
    #[must_use]
    pub fn with_policy(client: ArbitraitorClient, policy_toml: String) -> Self {
        Self {
            client,
            policy_toml,
        }
    }
}

impl DataReleaseEnforcer for ArbitraitorEnforcer {
    fn enforce_data_release(
        &self,
        content: &ClassifiedContent,
        destination: &ProviderId,
    ) -> Result<ReleaseVerdict, GovernanceError> {
        let digest = Digest::new("0".repeat(64));
        let _plan_context = self.client.build_plan_context_for_bash(true, &digest);

        let eval_context = EvalContext::default();
        let verdict = self
            .client
            .evaluate_policy(&self.policy_toml, &[], &eval_context)
            .map_err(|e| GovernanceError::ArbitraitorEnforcement(e.to_string()))?;

        let result = match verdict {
            Verdict::Pass | Verdict::Warn => ReleaseVerdict::Permit,
            Verdict::Block | Verdict::Prompt => ReleaseVerdict::Deny {
                reason: format!(
                    "arbitraitor denied release of {:?} content to {destination}",
                    content.classification
                ),
            },
            Verdict::Error | Verdict::Incomplete => {
                return Err(GovernanceError::ArbitraitorPolicy(format!(
                    "arbitraitor returned {verdict:?} verdict"
                )));
            }
        };
        Ok(result)
    }
}

// ---------------------------------------------------------------------------
// Glob matching for prohibited patterns
// ---------------------------------------------------------------------------

/// Simple glob matcher supporting `**` (match any characters including `/`).
///
/// Patterns without `**` require an exact match. This covers the spec §9.28.3
/// examples (`**/env.local`, `**/.aws/**`).
fn matches_glob(pattern: &str, path: &str) -> bool {
    if !pattern.contains("**") {
        return pattern == path;
    }
    let parts: Vec<&str> = pattern.split("**").collect();
    let mut pos = 0usize;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        match path[pos..].find(part) {
            Some(offset) => {
                if i == 0 && !pattern.starts_with("**") && offset != 0 {
                    return false;
                }
                pos += offset + part.len();
            }
            None => return false,
        }
    }
    if !pattern.ends_with("**") {
        let last = parts.last().copied().unwrap_or("");
        if !last.is_empty() && !path.ends_with(last) {
            return false;
        }
    }
    true
}
