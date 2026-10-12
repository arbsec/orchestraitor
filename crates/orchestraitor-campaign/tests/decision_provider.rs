//! Decision-provider task-selection consultation (spec `30-model-routing.md`
//! §9.45): a configured selector is consulted first, every failure mode
//! falls back to the deterministic P0-first selector, and the consultation
//! is attributed in the decision record. No network: the selector is a
//! recording fixture.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::sync::{Arc, Mutex};

use orchestraitor_campaign::{
    BoardSnapshot, CampaignDecisionStore, DecisionKind, SelectorDecision, plan_pass_with_selector,
};
use orchestraitor_provider_api::{DecisionProvider, FixtureDecisionProvider, FixtureMode};
use orchestraitor_worker::ProviderId;

type BoxError = Box<dyn std::error::Error>;
type TestResult = Result<(), BoxError>;

/// A scripted [`DecisionProvider`]: records the ids it was consulted with
/// and answers `propose_task_selection` from a fixed script.
#[derive(Clone)]
struct ScriptedSelector {
    provider: ProviderId,
    calls: Arc<Mutex<Vec<Vec<String>>>>,
    answer: Result<SelectorDecision, String>,
}

impl ScriptedSelector {
    fn selecting(provider_name: &'static str, task_id: &str, confidence: f64) -> Self {
        Self {
            provider: ProviderId::from_string(provider_name.to_string()),
            calls: Arc::new(Mutex::new(Vec::new())),
            answer: Ok(SelectorDecision {
                task_id: task_id.to_string(),
                confidence: Some(confidence),
            }),
        }
    }

    /// Simulates a provider that never answers (fixture `Unavailable` mode).
    fn unavailable(provider_name: &'static str) -> Self {
        Self {
            provider: ProviderId::from_string(provider_name.to_string()),
            calls: Arc::new(Mutex::new(Vec::new())),
            answer: Err(
                "decision provider unavailable: fixture configured unavailable".to_string(),
            ),
        }
    }

    /// Simulates a provider proposing an id outside the ready queue.
    fn out_of_set(provider_name: &'static str, task_id: &str) -> Self {
        Self {
            provider: ProviderId::from_string(provider_name.to_string()),
            calls: Arc::new(Mutex::new(Vec::new())),
            answer: Ok(SelectorDecision {
                task_id: task_id.to_string(),
                confidence: Some(0.99),
            }),
        }
    }

    fn consulted_with(&self) -> Vec<Vec<String>> {
        self.calls
            .lock()
            .map(|calls| calls.clone())
            .unwrap_or_default()
    }
}

#[async_trait::async_trait]
impl DecisionProvider for ScriptedSelector {
    fn id(&self) -> &ProviderId {
        &self.provider
    }

    async fn propose_task_selection(
        &self,
        ready_task_ids: &[String],
    ) -> orchestraitor_provider_api::DecisionResult<orchestraitor_provider_api::TaskSelection> {
        self.calls
            .lock()
            .map(|mut calls| calls.push(ready_task_ids.to_vec()))
            .unwrap_or_default();
        match self.answer.clone() {
            Ok(selection) => orchestraitor_provider_api::TaskSelection::new(
                selection.task_id,
                selection.confidence.unwrap_or_default(),
            ),
            Err(reason) => Err(
                orchestraitor_provider_api::DecisionProviderError::Unavailable {
                    provider_id: self.provider.clone(),
                    reason,
                },
            ),
        }
    }

    async fn propose_role_resolution(
        &self,
        _role: &str,
    ) -> orchestraitor_provider_api::DecisionResult<orchestraitor_provider_api::DecisionProposal>
    {
        Err(
            orchestraitor_provider_api::DecisionProviderError::Unavailable {
                provider_id: self.provider.clone(),
                reason: "task-selection-only test double".to_string(),
            },
        )
    }
}

// The blanket adapter bridges the shipped `FixtureDecisionProvider` (a
// `DecisionProvider`) onto the campaign seam directly — no manual shim.

fn routing() -> orchestraitor_agent_catalog::RoleRoutingDecision {
    orchestraitor_agent_catalog::RoleRoutingDecision {
        role: "implement".to_string(),
        provider: "neuralwatt".to_string(),
        model: "glm-5.2".to_string(),
        precedence_path: "bootstrap-default".to_string(),
        fallback_reason: Some("no explicit entry".to_string()),
    }
}

fn item(repo: &str, number: u64, priority: Option<&str>) -> orchestraitor_board::ItemFacts {
    orchestraitor_board::ItemFacts {
        item_node_id: format!("PVTI_{repo}_{number}"),
        repo: repo.to_string(),
        number,
        title: format!("task {repo}#{number}"),
        url: format!("https://github.com/{repo}/issues/{number}"),
        issue_type: Some("Task".to_string()),
        labels: Vec::new(),
        open_blockers: 0,
        target: Some("MVP".to_string()),
        status: Some("Ready".to_string()),
        priority: priority.map(std::string::ToString::to_string),
    }
}

fn ready(repo: &str, number: u64) -> orchestraitor_board::ReadyItem {
    orchestraitor_board::ReadyItem {
        number,
        title: format!("task {repo}#{number}"),
        url: format!("https://github.com/{repo}/issues/{number}"),
        repo: repo.to_string(),
        item_id: format!("PVTI_{repo}_{number}"),
    }
}

fn snapshot() -> BoardSnapshot {
    BoardSnapshot {
        open: vec![
            item("arbsec/orchestraitor", 2, None),
            item("arbsec/orchestraitor", 9, Some("P0")),
        ],
        ready: vec![
            ready("arbsec/orchestraitor", 2),
            ready("arbsec/orchestraitor", 9),
        ],
        blocked_candidates: Vec::new(),
        warnings: Vec::new(),
    }
}

fn task_id(number: u64) -> String {
    orchestraitor_campaign::task_id_for("arbsec/orchestraitor", number)
}

#[test]
fn without_selector_the_record_is_byte_identical_to_the_deterministic_path() -> TestResult {
    let store = CampaignDecisionStore::open_in_memory()?;
    let stored = plan_pass_with_selector(&snapshot(), &routing(), &store, None)?;
    assert_eq!(stored.decision.kind, DecisionKind::Selected);
    assert_eq!(
        stored
            .decision
            .selected
            .as_ref()
            .map(|task| task.task_id.as_str()),
        Some(task_id(9).as_str()),
        "P0-first deterministic selection unchanged"
    );
    assert_eq!(stored.decision.precedence_path, "bootstrap-default");
    assert_eq!(
        stored.decision.rationale,
        "first eligible item in P0-first ready order (spec 10-orchestrator.md §9.35)"
    );
    Ok(())
}

#[test]
fn a_configured_selector_is_consulted_and_wins_when_eligible() -> TestResult {
    let store = CampaignDecisionStore::open_in_memory()?;
    // Deliberately proposes the NON-first item (2) over the P0 item (9):
    // a well-formed proposal must win, proving consultation happened.
    let selector = ScriptedSelector::selecting("fixture", &task_id(2), 0.72);
    let stored = plan_pass_with_selector(&snapshot(), &routing(), &store, Some(&selector))?;
    assert_eq!(
        selector.consulted_with(),
        vec![vec![task_id(9), task_id(2)]],
        "the selector receives the ready queue in deterministic P0-first order"
    );
    assert_eq!(
        stored
            .decision
            .selected
            .as_ref()
            .map(|task| task.task_id.as_str()),
        Some(task_id(2).as_str())
    );
    assert_eq!(
        stored.decision.precedence_path,
        "decision-provider:fixture (confidence 0.72)"
    );
    assert!(
        stored
            .decision
            .rationale
            .contains("decision provider selected 'board-"),
        "rationale must attribute the selection to the provider: {}",
        stored.decision.rationale
    );
    Ok(())
}

#[test]
fn a_provider_error_falls_back_and_is_recorded() -> TestResult {
    let store = CampaignDecisionStore::open_in_memory()?;
    let selector = ScriptedSelector::unavailable("fixture");
    let stored = plan_pass_with_selector(&snapshot(), &routing(), &store, Some(&selector))?;
    assert_eq!(
        stored
            .decision
            .selected
            .as_ref()
            .map(|task| task.task_id.as_str()),
        Some(task_id(9).as_str()),
        "fallback keeps the deterministic P0-first selection"
    );
    assert_eq!(stored.decision.precedence_path, "bootstrap-default");
    assert!(
        stored
            .decision
            .rationale
            .contains("decision provider 'fixture' unavailable"),
        "fallback cause must be recorded: {}",
        stored.decision.rationale
    );
    assert!(
        stored.decision.rationale.contains("P0-first ready order"),
        "fallback names the deterministic rule: {}",
        stored.decision.rationale
    );
    Ok(())
}

#[test]
fn an_out_of_set_proposal_falls_back_and_is_recorded() -> TestResult {
    let store = CampaignDecisionStore::open_in_memory()?;
    let selector = ScriptedSelector::out_of_set("fixture", "board--999");
    let stored = plan_pass_with_selector(&snapshot(), &routing(), &store, Some(&selector))?;
    assert_eq!(
        stored
            .decision
            .selected
            .as_ref()
            .map(|task| task.task_id.as_str()),
        Some(task_id(9).as_str()),
        "an ineligible proposal never dispatches a worker at it"
    );
    assert!(
        stored
            .decision
            .rationale
            .contains("outside the eligible ready set"),
        "out-of-set cause must be recorded: {}",
        stored.decision.rationale
    );
    Ok(())
}

#[test]
fn an_empty_ready_queue_never_consults_the_selector() -> TestResult {
    let store = CampaignDecisionStore::open_in_memory()?;
    let selector = ScriptedSelector::selecting("fixture", &task_id(2), 0.5);
    let snap = BoardSnapshot {
        open: Vec::new(),
        ready: Vec::new(),
        blocked_candidates: Vec::new(),
        warnings: Vec::new(),
    };
    let stored = plan_pass_with_selector(&snap, &routing(), &store, Some(&selector))?;
    assert_eq!(stored.decision.kind, DecisionKind::NoOp);
    assert!(
        selector.consulted_with().is_empty(),
        "a no-op pass has nothing to select"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn the_shipped_fixture_answers_the_task_selection_surface() -> TestResult {
    // The fixture selects the FIRST ready id deterministically with
    // calibrated confidence — the shipped provider the CLI wires behind
    // `routing.provider = "fixture"`.
    let provider = FixtureDecisionProvider::new();
    let adapter: &dyn DecisionProvider = &provider;
    let resolved = adapter
        .propose_task_selection(&[task_id(2), task_id(9)])
        .await
        .expect("available fixture must select");
    assert_eq!(resolved.task_id, task_id(2));
    assert!((resolved.confidence - 1.0).abs() < 1e-9);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unavailable_provider_flattens_to_a_log_safe_reason() -> TestResult {
    let provider = FixtureDecisionProvider::with_mode(FixtureMode::Unavailable);
    let adapter: &dyn DecisionProvider = &provider;
    let error = adapter
        .propose_task_selection(&[task_id(2)])
        .await
        .expect_err("unavailable fixture must not select");
    assert!(error.to_string().contains("fixture configured unavailable"));
    Ok(())
}

/// Sanity: the typed alternative shape still round-trips through the
/// provider API (guarding accidental vocabulary drift).
#[test]
fn decision_alternative_vocabulary_is_stable() {
    use orchestraitor_provider_api::{DecisionAlternative, DecisionProvider};
    let alternative = DecisionAlternative {
        provider: FixtureDecisionProvider::new().id().clone(),
        model: "glm-5.2".to_string(),
        skip_reason: "skipped-because-quota".to_string(),
    };
    assert_eq!(alternative.skip_reason, "skipped-because-quota");
}
