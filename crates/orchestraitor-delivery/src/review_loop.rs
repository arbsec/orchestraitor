//! Configurable review-loop parameters (spec §9.33.4).
//!
//! Completion of an implementation change set triggers an adversarial review
//! loop: reviewers in fresh contexts produce findings, findings are
//! consolidated and remediated, and the loop repeats until no blocking
//! findings remain or a configured limit is reached. This module defines the
//! tunable parameters of that loop and their spec-mandated defaults.
//!
//! Security boundary (spec §9.33.7): this module only structures
//! configuration. It MUST NOT make security decisions (allow/deny/verdict) —
//! blocking decisions are made by the runner and policy layer, and all policy,
//! approval, promotion, and receipt authority belongs to Arbitraitor (§2.2).

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::metadata::DomainId;

/// Finding severity for review-loop blocking decisions (spec §9.33.4
/// `minimum_severity_to_block`). Ordered: `Minimal < Suggestion < Low <
/// Medium < High < Critical`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Cosmetic or informational; never blocks.
    Minimal,
    /// Advisory suggestion; never blocks by default.
    Suggestion,
    /// Minor defect; blocks only under the strictest configuration.
    Low,
    /// Moderate defect.
    Medium,
    /// Serious defect; the spec default blocking threshold (§9.33.4).
    High,
    /// Release-blocking defect under any configuration.
    Critical,
}

impl Severity {
    /// Returns true when this finding severity meets or exceeds the configured
    /// `minimum_severity_to_block` threshold. The verdict itself is rendered
    /// by the runner/policy layer (§9.33.7), not here.
    #[must_use]
    pub fn blocks(&self, minimum_to_block: &Self) -> bool {
        self >= minimum_to_block
    }
}

/// Default for [`ReviewLoopConfig::max_review_loops`] (§9.33.4: 3).
fn default_max_review_loops() -> u32 {
    3
}

/// Default for [`ReviewLoopConfig::max_reviewers`] (§9.33.4: 5).
fn default_max_reviewers() -> u32 {
    5
}

/// Default for [`ReviewLoopConfig::required_reviewer_domains`] (§9.33.4:
/// `["security"]` for security-sensitive tasks).
fn default_required_reviewer_domains() -> Vec<DomainId> {
    vec![DomainId::new("security")]
}

/// Default for [`ReviewLoopConfig::minimum_severity_to_block`] (§9.33.4:
/// `"high"`).
fn default_minimum_severity_to_block() -> Severity {
    Severity::High
}

/// Default for [`ReviewLoopConfig::stop_when_no_blocking_findings`]
/// (§9.33.4: true).
fn default_stop_when_no_blocking_findings() -> bool {
    true
}

/// Structural validation failure for [`ReviewLoopConfig`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ReviewLoopConfigError {
    /// `max_review_loops` is zero; the loop could never run.
    #[error("max_review_loops must be at least 1")]
    ZeroMaxReviewLoops,
    /// `max_reviewers` is zero; no reviewer could ever be selected.
    #[error("max_reviewers must be at least 1")]
    ZeroMaxReviewers,
    /// A required reviewer domain entry is empty or whitespace.
    #[error("required_reviewer_domains must not contain blank entries")]
    BlankReviewerDomain,
}

/// Configurable parameters of the §9.33.4 review loop.
///
/// This is the struct that `[[delivery.review_loop]]`-style configuration
/// sections deserialize into. Missing keys fall back to the spec-mandated
/// defaults; unknown keys are rejected so configuration drift fails visibly.
// allow: the booleans are independent spec-mandated knobs (§9.33.4), not a state machine.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ReviewLoopConfig {
    /// Maximum number of review/remediate iterations before the loop MUST
    /// produce an explicit `blocked` or `needs-human` state (§9.24, §9.33.4).
    /// Default: 3.
    #[serde(default = "default_max_review_loops")]
    pub max_review_loops: u32,
    /// Maximum number of reviewers selected per loop. Default: 5.
    #[serde(default = "default_max_reviewers")]
    pub max_reviewers: u32,
    /// Reviewer domains that must cover the change set. Default:
    /// `["security"]` for security-sensitive tasks (§9.33.4).
    #[serde(default = "default_required_reviewer_domains")]
    pub required_reviewer_domains: Vec<DomainId>,
    /// Findings at or above this severity block convergence. Default:
    /// [`Severity::High`] (§9.33.4).
    #[serde(default = "default_minimum_severity_to_block")]
    pub minimum_severity_to_block: Severity,
    /// Whether the implementing model may also review. Default: false.
    pub allow_same_model: bool,
    /// Whether reviewers must span distinct providers. Default: false.
    pub require_provider_diversity: bool,
    /// Whether human review is mandatory regardless of automated reviewer
    /// output (true for security-sensitive changes, §21.1). Default: false.
    pub require_human_review: bool,
    /// Stop the loop as soon as no blocking findings remain. Default: true.
    #[serde(default = "default_stop_when_no_blocking_findings")]
    pub stop_when_no_blocking_findings: bool,
}

impl Default for ReviewLoopConfig {
    fn default() -> Self {
        Self {
            max_review_loops: default_max_review_loops(),
            max_reviewers: default_max_reviewers(),
            required_reviewer_domains: default_required_reviewer_domains(),
            minimum_severity_to_block: default_minimum_severity_to_block(),
            allow_same_model: false,
            require_provider_diversity: false,
            require_human_review: false,
            stop_when_no_blocking_findings: default_stop_when_no_blocking_findings(),
        }
    }
}

impl ReviewLoopConfig {
    /// Configuration for security-sensitive changes (§9.33.4, §21.1): the
    /// `security` reviewer domain is required and human review is mandatory
    /// regardless of automated reviewer output.
    #[must_use]
    pub fn for_security_sensitive() -> Self {
        Self {
            require_human_review: true,
            ..Self::default()
        }
    }

    /// Validates structural invariants: the loop must be able to run at least
    /// once, at least one reviewer must be selectable, and required reviewer
    /// domains must not contain blank entries.
    ///
    /// # Errors
    ///
    /// Returns the first [`ReviewLoopConfigError`] encountered.
    pub fn validate(&self) -> Result<(), ReviewLoopConfigError> {
        if self.max_review_loops == 0 {
            return Err(ReviewLoopConfigError::ZeroMaxReviewLoops);
        }
        if self.max_reviewers == 0 {
            return Err(ReviewLoopConfigError::ZeroMaxReviewers);
        }
        if self
            .required_reviewer_domains
            .iter()
            .any(|d| d.as_str().trim().is_empty())
        {
            return Err(ReviewLoopConfigError::BlankReviewerDomain);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn defaults_match_spec_table() {
        let config = ReviewLoopConfig::default();
        assert_eq!(config.max_review_loops, 3);
        assert_eq!(config.max_reviewers, 5);
        assert_eq!(
            config.required_reviewer_domains,
            vec![DomainId::new("security")]
        );
        assert_eq!(config.minimum_severity_to_block, Severity::High);
        assert!(!config.allow_same_model);
        assert!(!config.require_provider_diversity);
        assert!(!config.require_human_review);
        assert!(config.stop_when_no_blocking_findings);
    }

    #[test]
    fn security_sensitive_constructor_requires_human_review() -> TestResult {
        let config = ReviewLoopConfig::for_security_sensitive();
        assert!(config.require_human_review);
        assert_eq!(
            config.required_reviewer_domains,
            vec![DomainId::new("security")]
        );
        assert_eq!(config.max_review_loops, 3);
        config.validate()?;
        Ok(())
    }

    #[test]
    fn default_config_is_valid() -> TestResult {
        ReviewLoopConfig::default().validate()?;
        Ok(())
    }

    #[test]
    fn zero_max_review_loops_is_rejected() {
        let config = ReviewLoopConfig {
            max_review_loops: 0,
            ..ReviewLoopConfig::default()
        };
        assert_eq!(
            config.validate(),
            Err(ReviewLoopConfigError::ZeroMaxReviewLoops)
        );
    }

    #[test]
    fn zero_max_reviewers_is_rejected() {
        let config = ReviewLoopConfig {
            max_reviewers: 0,
            ..ReviewLoopConfig::default()
        };
        assert_eq!(
            config.validate(),
            Err(ReviewLoopConfigError::ZeroMaxReviewers)
        );
    }

    #[test]
    fn blank_reviewer_domain_is_rejected() {
        let config = ReviewLoopConfig {
            required_reviewer_domains: vec![DomainId::new("backend"), DomainId::new("  ")],
            ..ReviewLoopConfig::default()
        };
        assert_eq!(
            config.validate(),
            Err(ReviewLoopConfigError::BlankReviewerDomain)
        );
    }

    #[test]
    fn empty_object_deserializes_to_spec_defaults() -> TestResult {
        let config: ReviewLoopConfig = serde_json::from_str("{}")?;
        assert_eq!(config, ReviewLoopConfig::default());
        Ok(())
    }

    #[test]
    fn config_round_trips_through_json() -> TestResult {
        let config = ReviewLoopConfig {
            max_review_loops: 7,
            max_reviewers: 2,
            required_reviewer_domains: vec![DomainId::new("backend"), DomainId::new("security")],
            minimum_severity_to_block: Severity::Critical,
            allow_same_model: true,
            require_provider_diversity: true,
            require_human_review: true,
            stop_when_no_blocking_findings: false,
        };
        let json = serde_json::to_string_pretty(&config)?;
        let back: ReviewLoopConfig = serde_json::from_str(&json)?;
        assert_eq!(config, back);
        back.validate()?;
        Ok(())
    }

    #[test]
    fn serialization_omits_nothing_load_bearing() -> TestResult {
        let json = serde_json::to_string(&ReviewLoopConfig::default())?;
        for key in [
            "max_review_loops",
            "max_reviewers",
            "required_reviewer_domains",
            "minimum_severity_to_block",
            "allow_same_model",
            "require_provider_diversity",
            "require_human_review",
            "stop_when_no_blocking_findings",
        ] {
            assert!(json.contains(key), "serialized config must contain {key}");
        }
        Ok(())
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let json = r#"{"max_review_loops": 3, "surprise_key": true}"#;
        assert!(serde_json::from_str::<ReviewLoopConfig>(json).is_err());
    }

    #[test]
    fn severity_orders_minimal_to_critical() {
        let ordered = [
            Severity::Minimal,
            Severity::Suggestion,
            Severity::Low,
            Severity::Medium,
            Severity::High,
            Severity::Critical,
        ];
        for pair in ordered.windows(2) {
            assert!(pair[0] < pair[1]);
        }
    }

    #[test]
    fn blocks_at_and_above_minimum_only() {
        let min = Severity::High;
        assert!(!Severity::Minimal.blocks(&min));
        assert!(!Severity::Suggestion.blocks(&min));
        assert!(!Severity::Low.blocks(&min));
        assert!(!Severity::Medium.blocks(&min));
        assert!(Severity::High.blocks(&min));
        assert!(Severity::Critical.blocks(&min));
        // Boundary: threshold at the extremes.
        assert!(Severity::Minimal.blocks(&Severity::Minimal));
        assert!(!Severity::High.blocks(&Severity::Critical));
    }

    #[test]
    fn severity_serializes_as_snake_case() -> TestResult {
        let json = serde_json::to_string(&Severity::High)?;
        assert_eq!(json, r#""high""#);
        let back: Severity = serde_json::from_str(r#""high""#)?;
        assert_eq!(back, Severity::High);
        assert!(serde_json::from_str::<Severity>(r#""severe""#).is_err());
        Ok(())
    }
}
