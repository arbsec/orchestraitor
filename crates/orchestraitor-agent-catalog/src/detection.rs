//! Weighted domain detection heuristics backed by embedded TOML rules.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::AgentCatalogError;

const BUILT_IN_DETECTION_RULES: &str = include_str!("detection_rules.toml");

/// Project artifact presented to the detection engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DetectionArtifact<'a> {
    /// Repository-relative artifact path using `/` separators.
    pub path: &'a str,
    /// Optional textual contents for content-weighted rules.
    pub contents: Option<&'a str>,
}

/// Parsed detection rule set and score threshold.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct DetectionRuleSet {
    /// Minimum total score needed before a domain is returned.
    pub threshold: u32,
    /// Weighted rules grouped by output domain.
    pub rules: Vec<DetectionRule>,
}

/// One weighted rule contributing to one domain.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct DetectionRule {
    /// Domain id that receives the rule's weight.
    pub domain: String,
    /// Weight added when the rule matches.
    pub weight: u32,
    /// Match predicate.
    #[serde(flatten)]
    pub matcher: DetectionMatcher,
}

/// Match predicate for project artifact signals.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DetectionMatcher {
    /// Matches an exact repository-relative path.
    PathExact(String),
    /// Matches a repository-relative path prefix.
    PathPrefix(String),
    /// Matches a repository-relative path suffix.
    PathSuffix(String),
    /// Matches a substring in a repository-relative path.
    PathContains(String),
    /// Matches a substring in optional artifact contents.
    ContentContains(String),
}

/// Domain selected by detection with its winning score.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedDomain {
    /// Selected domain id, or `general` when below threshold.
    pub domain: String,
    /// Winning score; zero for the `general` fallback.
    pub score: u32,
}

/// Weighted signal matcher for project-domain detection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detector {
    rules: DetectionRuleSet,
}

impl DetectionRuleSet {
    /// Parses the embedded built-in TOML detection rules.
    ///
    /// # Errors
    ///
    /// Returns an error when the embedded TOML is invalid.
    pub fn built_in() -> Result<Self, AgentCatalogError> {
        Ok(toml::from_str(BUILT_IN_DETECTION_RULES)?)
    }
}

impl Detector {
    /// Creates a detector from explicit rules.
    #[must_use]
    pub const fn new(rules: DetectionRuleSet) -> Self {
        Self { rules }
    }

    /// Creates a detector from embedded built-in rules.
    ///
    /// # Errors
    ///
    /// Returns an error when the embedded TOML is invalid.
    pub fn built_in() -> Result<Self, AgentCatalogError> {
        Ok(Self::new(DetectionRuleSet::built_in()?))
    }

    /// Detects the most likely domain from weighted local artifacts.
    #[must_use]
    pub fn detect<'a>(
        &self,
        artifacts: impl IntoIterator<Item = DetectionArtifact<'a>>,
    ) -> DetectedDomain {
        let artifacts = artifacts.into_iter().collect::<Vec<_>>();
        let mut scores = BTreeMap::<&str, u32>::new();
        for rule in &self.rules.rules {
            if artifacts
                .iter()
                .any(|artifact| rule.matcher.matches(artifact))
            {
                let score = scores.entry(rule.domain.as_str()).or_default();
                *score = score.saturating_add(rule.weight);
            }
        }
        let Some((domain, score)) = scores
            .into_iter()
            .max_by_key(|(domain, score)| (*score, *domain))
        else {
            return general();
        };
        if score < self.rules.threshold {
            return general();
        }
        DetectedDomain {
            domain: domain.to_string(),
            score,
        }
    }
}

impl DetectionMatcher {
    fn matches(&self, artifact: &DetectionArtifact<'_>) -> bool {
        match self {
            Self::PathExact(value) => artifact.path == value,
            Self::PathPrefix(value) => artifact.path.starts_with(value),
            Self::PathSuffix(value) => artifact.path.ends_with(value),
            Self::PathContains(value) => artifact.path.contains(value),
            Self::ContentContains(value) => {
                artifact.contents.is_some_and(|body| body.contains(value))
            }
        }
    }
}

fn general() -> DetectedDomain {
    DetectedDomain {
        domain: "general".to_string(),
        score: 0,
    }
}
