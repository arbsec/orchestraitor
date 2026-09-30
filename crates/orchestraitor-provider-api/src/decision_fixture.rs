//! Deterministic table-driven [`DecisionProvider`] fixture (spec §21.3): a
//! conformance target for the `DecisionProvider` trait and the only shipped
//! implementation until the TypeSafe/jev adapter is allowlisted (tech-stack
//! §17, §18). CI never depends on a live decision model (spec
//! `50-contracts-data.md` §21.3); this fixture is fully offline and
//! deterministic.
//!
//! The fixture maps role ids to proposals from a static table. Unknown roles
//! are a typed [`DecisionProviderError::Unavailable`] — the router treats
//! that exactly like any other unavailability and falls back to the
//! heuristic table, so no role-specific special case exists on the caller
//! side. The fixture can be configured to error, which exercises the
//! fallback chain (issue #329 failure scenario).

use async_trait::async_trait;
use orchestraitor_model::ProviderId;

use crate::decision::{
    DecisionAlternative, DecisionProposal, DecisionProvider, DecisionResult, TaskSelection,
};
use crate::error::DecisionProviderError;

/// Built-in fixture proposal table entry.
struct FixtureEntry {
    role: &'static str,
    provider: &'static str,
    model: &'static str,
    confidence: f64,
    alternatives: &'static [(&'static str, &'static str, &'static str)],
}

/// The static fixture table: every built-in orchestration role (spec §9.45)
/// resolves to the same single-provider bootstrap default the heuristic table
/// ships, with one alternative carrying a quota skip reason so downstream
/// consumers can exercise the alternatives shape (spec §9.45, §9.46).
static FIXTURE_TABLE: &[FixtureEntry] = &[
    FixtureEntry {
        role: "explore",
        provider: "neuralwatt",
        model: "glm-5.2",
        confidence: 0.9,
        alternatives: &[(
            "neuralwatt",
            "glm-5.2-flash",
            "skipped-because-lower-margin",
        )],
    },
    FixtureEntry {
        role: "research",
        provider: "neuralwatt",
        model: "glm-5.2",
        confidence: 0.85,
        alternatives: &[(
            "neuralwatt",
            "glm-5.2-flash",
            "skipped-because-lower-margin",
        )],
    },
    FixtureEntry {
        role: "plan",
        provider: "neuralwatt",
        model: "glm-5.2",
        confidence: 0.8,
        alternatives: &[(
            "neuralwatt",
            "glm-5.2-flash",
            "skipped-because-lower-margin",
        )],
    },
    FixtureEntry {
        role: "implement",
        provider: "neuralwatt",
        model: "glm-5.2",
        confidence: 0.95,
        alternatives: &[(
            "neuralwatt",
            "glm-5.2-flash",
            "skipped-because-lower-margin",
        )],
    },
    FixtureEntry {
        role: "review",
        provider: "neuralwatt",
        model: "glm-5.2",
        confidence: 0.88,
        alternatives: &[(
            "neuralwatt",
            "glm-5.2-flash",
            "skipped-because-lower-margin",
        )],
    },
    FixtureEntry {
        role: "verify",
        provider: "neuralwatt",
        model: "glm-5.2",
        confidence: 0.87,
        alternatives: &[(
            "neuralwatt",
            "glm-5.2-flash",
            "skipped-because-lower-margin",
        )],
    },
];

/// Whether the fixture serves proposals or simulates unavailability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixtureMode {
    /// Serve proposals from the static table.
    Available,
    /// Error on every request, simulating an unavailable decision model.
    Unavailable,
}

/// Deterministic table-driven `DecisionProvider` fixture.
#[derive(Debug, Clone)]
pub struct FixtureDecisionProvider {
    id: ProviderId,
    mode: FixtureMode,
}

impl FixtureDecisionProvider {
    /// Creates an available fixture serving the static table.
    #[must_use]
    pub fn new() -> Self {
        Self {
            id: ProviderId::from_string("fixture".to_string()),
            mode: FixtureMode::Available,
        }
    }

    /// Creates a fixture in the given mode (`Unavailable` simulates a
    /// decision model that errors on every request).
    #[must_use]
    pub fn with_mode(mode: FixtureMode) -> Self {
        Self {
            id: ProviderId::from_string("fixture".to_string()),
            mode,
        }
    }
}

impl Default for FixtureDecisionProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl DecisionProvider for FixtureDecisionProvider {
    fn id(&self) -> &ProviderId {
        &self.id
    }

    async fn propose_role_resolution(&self, role: &str) -> DecisionResult<DecisionProposal> {
        if self.mode == FixtureMode::Unavailable {
            return Err(DecisionProviderError::Unavailable {
                provider_id: self.id.clone(),
                reason: "fixture configured unavailable".to_string(),
            });
        }
        let entry = FIXTURE_TABLE.iter().find(|entry| entry.role == role);
        let Some(entry) = entry else {
            return Err(DecisionProviderError::Unavailable {
                provider_id: self.id.clone(),
                reason: format!("no fixture table entry for role '{role}'"),
            });
        };
        let alternatives = entry
            .alternatives
            .iter()
            .map(|(provider, model, skip_reason)| DecisionAlternative {
                provider: ProviderId::from_string((*provider).to_string()),
                model: (*model).to_string(),
                skip_reason: (*skip_reason).to_string(),
            })
            .collect();
        DecisionProposal::new(
            entry.role,
            ProviderId::from_string(entry.provider.to_string()),
            entry.model,
            entry.confidence,
            alternatives,
        )
    }

    async fn propose_task_selection(
        &self,
        ready_task_ids: &[String],
    ) -> DecisionResult<TaskSelection> {
        if self.mode == FixtureMode::Unavailable {
            return Err(DecisionProviderError::Unavailable {
                provider_id: self.id.clone(),
                reason: "fixture configured unavailable".to_string(),
            });
        }
        let Some(first) = ready_task_ids.first() else {
            return Err(DecisionProviderError::Unavailable {
                provider_id: self.id.clone(),
                reason: "empty ready queue".to_string(),
            });
        };
        TaskSelection::new(first.clone(), 1.0)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;

    #[tokio::test]
    async fn fixture_serves_table_entries_for_all_built_in_roles() {
        let fixture = FixtureDecisionProvider::new();
        for role in [
            "explore",
            "research",
            "plan",
            "implement",
            "review",
            "verify",
        ] {
            let proposal = fixture.propose_role_resolution(role).await.unwrap();
            assert_eq!(proposal.role, role);
            assert_eq!(proposal.provider.as_str(), "neuralwatt");
            assert_eq!(proposal.model, "glm-5.2");
            assert!(
                (0.0..=1.0).contains(&proposal.confidence),
                "confidence must be calibrated"
            );
            assert!(!proposal.alternatives.is_empty());
        }
    }

    #[tokio::test]
    async fn fixture_unknown_role_is_typed_unavailable() {
        let fixture = FixtureDecisionProvider::new();
        let error = fixture
            .propose_role_resolution("nonexistent")
            .await
            .unwrap_err();
        assert!(matches!(error, DecisionProviderError::Unavailable { .. }));
    }

    #[tokio::test]
    async fn fixture_unavailable_mode_errors_typed() {
        let fixture = FixtureDecisionProvider::with_mode(FixtureMode::Unavailable);
        let error = fixture.propose_role_resolution("plan").await.unwrap_err();
        assert!(matches!(error, DecisionProviderError::Unavailable { .. }));
        let error = fixture
            .propose_task_selection(&["task-a".to_string()])
            .await
            .unwrap_err();
        assert!(matches!(error, DecisionProviderError::Unavailable { .. }));
    }

    #[tokio::test]
    async fn fixture_selects_first_ready_task_deterministically() {
        let fixture = FixtureDecisionProvider::new();
        let selection = fixture
            .propose_task_selection(&["task-b".to_string(), "task-a".to_string()])
            .await
            .unwrap();
        assert_eq!(selection.task_id, "task-b");
        assert!((selection.confidence - 1.0).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn fixture_empty_ready_queue_is_typed_unavailable() {
        let fixture = FixtureDecisionProvider::new();
        let error = fixture.propose_task_selection(&[]).await.unwrap_err();
        assert!(matches!(error, DecisionProviderError::Unavailable { .. }));
    }

    #[tokio::test]
    async fn fixture_is_trait_object_safe() {
        let fixture = FixtureDecisionProvider::new();
        let provider: &dyn DecisionProvider = &fixture;
        let proposal = provider.propose_role_resolution("implement").await.unwrap();
        assert_eq!(provider.id().as_str(), "fixture");
        assert_eq!(proposal.role, "implement");
    }
}
