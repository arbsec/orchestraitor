//! The pass itself: order, select, record, spawn. Pure over the board
//! snapshot supplied by the caller; the only side effects are the single
//! decision-store insert and the injected spawner call.

use orchestraitor_agent_catalog::RoleRoutingDecision;
use orchestraitor_board::{ItemFacts, ReadyItem, SkipWarning, WarningKind};
use orchestraitor_provider_api::DecisionProvider;
use orchestraitor_worker::WorkerRun;

use crate::decision::{
    BlockedNode, CampaignDecision, CampaignDecisionStore, DecisionKind, NoOpReason, SelectedTask,
    SkipRecord, StoredCampaignDecision,
};
use crate::error::CampaignError;

/// The typed output of a provider consultation (the campaign's
/// task-selection seam, spec `30-model-routing.md` §9.45): a configured
/// [`DecisionProvider`] is consulted first via its `propose_task_selection`
/// surface; any error — unavailability, malformed output, an id outside the
/// ready queue — engages the deterministic P0-first selector, which stays
/// the fallback chain. Carries the chosen ready-queue task id and the
/// selector's calibrated confidence.
#[derive(Debug, Clone, PartialEq)]
pub struct SelectorDecision {
    /// The selected deterministic worker task id.
    pub task_id: String,
    /// Calibrated confidence in `0.0..=1.0`, when the selector reports one.
    pub confidence: Option<f64>,
}

/// Consults a decision provider's task-selection surface. The ASYNC form
/// ([`consult_task_selection_async`]) is the production path — the loop
/// runner awaits it so supervision and the shutdown race keep running
/// during the provider call. The synchronous form below exists only for
/// the one-shot CLI caller, which has no ambient runtime context to await
/// in and must not spin one up inside an async context.
fn consult_task_selection(
    provider: &dyn DecisionProvider,
    ready_task_ids: &[String],
) -> Result<SelectorDecision, String> {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        return handle.block_on(consult_task_selection_async(provider, ready_task_ids));
    }
    tokio::runtime::Runtime::new()
        .map_err(|error| format!("decision-provider runtime build failed: {error}"))?
        .block_on(consult_task_selection_async(provider, ready_task_ids))
}

/// The async consultation: awaits the provider call directly (never blocks
/// a worker thread), so loop supervision, the shutdown race, and the
/// run-budget check keep running while the endpoint answers. Bounded by
/// [`DECISION_CONSULT_TIMEOUT`] — a slow endpoint degrades to the
/// deterministic selector, never a stalled loop.
async fn consult_task_selection_async(
    provider: &dyn DecisionProvider,
    ready_task_ids: &[String],
) -> Result<SelectorDecision, String> {
    let flatten = |selection: orchestraitor_provider_api::TaskSelection| SelectorDecision {
        task_id: selection.task_id,
        confidence: Some(selection.confidence),
    };
    match tokio::time::timeout(
        DECISION_CONSULT_TIMEOUT,
        provider.propose_task_selection(ready_task_ids),
    )
    .await
    {
        Ok(Ok(selection)) => Ok(flatten(selection)),
        Ok(Err(error)) => Err(error.to_string()),
        Err(_) => Err(format!(
            "decision provider did not answer within \
             {DECISION_CONSULT_TIMEOUT:?}; fell back to the deterministic selector"
        )),
    }
}

/// Upper bound on one decision-provider consultation (spec
/// `30-model-routing.md` §9.45): a slow
/// or hung endpoint degrades to the deterministic selector instead of
/// stalling loop supervision past the shutdown budget.
pub const DECISION_CONSULT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Spawns the worker for a selected task on the daemon-less direct path.
/// Production wires the bootstrap transport (fixture task source, mediated
/// bash, pending delivery sink); tests inject a fake — the trait is the
/// hermeticity seam.
pub trait WorkerSpawner {
    /// Runs one leaf task end-to-end and returns the structured result.
    ///
    /// # Errors
    ///
    /// Returns [`CampaignError::Spawn`] when no run can be produced at all
    /// (unusable worktree, transport failure). Classifiable worker failures
    /// arrive inside the returned [`WorkerRun`] as typed failures.
    fn spawn(
        &self,
        task_id: &str,
        routing: &RoleRoutingDecision,
    ) -> Result<WorkerRun, CampaignError>;
}

/// The reconciled board state one pass reads: every open item in the
/// configured repositories, the ready queue over it, the eligible candidates
/// blocked by unresolved dependencies, and the warning channel (closed items
/// surface as `NotOpen` warnings).
#[derive(Debug, Clone)]
pub struct BoardSnapshot {
    /// All open items in scope.
    pub open: Vec<ItemFacts>,
    /// Ready-queue items (leaf, MVP, Ready, no unresolved blockers), already
    /// sorted by issue number (as [`ready_queue`] produces them); the
    /// selection pass applies the P0-first epic-focus order on top.
    pub ready: Vec<ReadyItem>,
    /// Eligible candidates whose only disqualifier is unresolved blockers
    /// (spec §9.35 "all-blocked"), sorted by `(repo, issue number)`.
    pub blocked_candidates: Vec<ReadyItem>,
    /// Warnings for items that could not be evaluated; `NotOpen` entries are
    /// what separates [`NoOpReason::EpicExhausted`] from
    /// [`NoOpReason::EmptyQueue`].
    pub warnings: Vec<SkipWarning>,
}

/// The result of one pass: the persisted decision plus the worker run, when a
/// task was selected and spawned.
#[derive(Debug)]
pub struct CampaignOutcome {
    /// The exactly-one decision record for this pass.
    pub decision: StoredCampaignDecision,
    /// The worker run, present iff the pass selected a task and the spawn
    /// produced a run.
    pub worker: Option<WorkerRun>,
}

/// Worker-task-id charset limit from `orchestraitor-worker`'s id validation
/// (`task.rs`): the id becomes a file name, so it is capped at 64 bytes.
const TASK_ID_MAX_LEN: usize = 64;

/// Deterministic worker task id for a selected board item. Issue numbers
/// are per-repo, so the identity must carry the repo — two configured repos
/// with the same issue number must never load the same fixture. The
/// readable `board-<owner>_<repo>-<number>` form exists only for
/// charset-clean `owner/name` repos: `_` cannot occur inside either part,
/// so the id decomposes uniquely back into `(owner, name, number)` and
/// dash-position variants (`foo/bar-baz` vs `foo-bar/baz`) stay distinct.
/// Any other repo (punctuation, extra slashes, bare names) and any slug
/// overflowing the 64-byte file-name bound falls back to a truncated slug
/// plus an 8-hex FNV-1a digest of the full repo; that digest path never
/// emits `_`, so it cannot collide with the readable form, and 32-bit
/// collision resistance is ample for operator-configured repo counts
/// (birthday bound ~2^16 repos) on configuration, never attacker content.
#[must_use]
pub fn task_id_for(repo: &str, number: u64) -> String {
    let prefix = format!("board--{number}");
    let budget = TASK_ID_MAX_LEN.saturating_sub(prefix.len());
    let charset_clean = |part: &str| {
        !part.is_empty() && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    };
    if let Some((owner, name)) = repo.split_once('/')
        && charset_clean(owner)
        && charset_clean(name)
    {
        let owner_name: String = repo
            .chars()
            .map(|c| if c == '/' { '_' } else { c })
            .collect();
        if owner_name.len() <= budget {
            return format!("board-{owner_name}-{number}");
        }
    }
    let slug: String = repo
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let digest = format!("{:08x}", fnv1a(repo));
    let truncated: String = slug.chars().take(budget.saturating_sub(9)).collect();
    format!("board-{truncated}-{digest}-{number}")
}

/// Stable 32-bit FNV-1a digest over the full repo string; pure ASCII output
/// keeps ids byte- and char-length identical.
fn fnv1a(value: &str) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in value.as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

/// Applies the minimal epic-focus rule (spec §9.41): candidates whose
/// configured priority-field value is `P0` first, stable `(repo, issue
/// number)` order within each group. The priority field is authoritative —
/// labels are advisory per the board config.
#[must_use]
pub fn compute_selection(ready: &[ReadyItem], open: &[ItemFacts]) -> Vec<ReadyItem> {
    let mut ordered: Vec<&ReadyItem> = ready.iter().collect();
    ordered.sort_by_key(|item| {
        let facts = open
            .iter()
            .find(|facts| facts.repo == item.repo && facts.number == item.number);
        let p0 = facts.is_some_and(|facts| {
            facts
                .priority
                .as_deref()
                .is_some_and(|priority| priority.eq_ignore_ascii_case("p0"))
        });
        (u8::from(!p0), item.repo.clone(), item.number)
    });
    ordered.into_iter().cloned().collect()
}

/// Plans exactly one pass without spawning: reads the snapshot, selects at
/// most one task, and persists exactly one decision record. No-op passes
/// persist their typed record. The loop runner (issue #314) uses this to
/// separate selection from background worker supervision; [`run_once`] is
/// `plan_pass` plus the synchronous spawn. Without a configured decision
/// provider this is byte-identical to the pre-§9.45 behavior.
///
/// # Errors
///
/// Returns [`CampaignError::Store`] when the record cannot be persisted.
pub fn plan_pass(
    snapshot: &BoardSnapshot,
    routing: &RoleRoutingDecision,
    store: &CampaignDecisionStore,
) -> Result<StoredCampaignDecision, CampaignError> {
    plan_pass_with_selector(snapshot, routing, store, None)
}

/// [`plan_pass`] with an optional configured decision provider (spec
/// `30-model-routing.md` §9.45): `Some` consults the provider for task
/// selection first and falls back to the deterministic P0-first selector on
/// any provider error. The SYNCHRONOUS form — for the one-shot CLI caller
/// (`run_once_with_selector`), which has no ambient async context. The loop
/// runner uses [`plan_pass_with_selector_async`], which awaits the
/// consultation so supervision and the shutdown race keep running.
///
/// # Errors
///
/// Returns [`CampaignError::Store`] when the record cannot be persisted.
pub fn plan_pass_with_selector(
    snapshot: &BoardSnapshot,
    routing: &RoleRoutingDecision,
    store: &CampaignDecisionStore,
    selector: Option<&dyn DecisionProvider>,
) -> Result<StoredCampaignDecision, CampaignError> {
    let decision = campaign_decision_sync(snapshot, routing, selector);
    store.record(&decision)
}

/// [`plan_pass_with_selector`] for async callers (the loop runner): the
/// provider consultation is awaited — bounded by
/// [`DECISION_CONSULT_TIMEOUT`] — so loop supervision and the shutdown race
/// keep running while the decision endpoint answers. Same fallback
/// semantics and decision-record shape as the synchronous form.
///
/// # Errors
///
/// Returns [`CampaignError::Store`] when the record cannot be persisted.
pub async fn plan_pass_with_selector_async(
    snapshot: &BoardSnapshot,
    routing: &RoleRoutingDecision,
    store: &CampaignDecisionStore,
    selector: Option<&dyn DecisionProvider>,
) -> Result<StoredCampaignDecision, CampaignError> {
    let decision = campaign_decision_async(snapshot, routing, selector).await;
    store.record(&decision)
}

/// Constructs the exactly-one §9.35 decision record for a pass: the
/// Selected branch for the first eligible item, else the typed no-op
/// classification. The single construction path shared by the sync and
/// async planners — the two consumers can never drift apart. With a
/// configured selector (spec `30-model-routing.md` §9.45) a well-formed
/// proposal selects its task and the confidence lands in `precedence_path`;
/// a provider error, an id outside the eligible set, or a missing selector
/// falls back to the deterministic P0-first selection and the cause is
/// recorded in `rationale`.
fn campaign_decision(
    snapshot: &BoardSnapshot,
    routing: &RoleRoutingDecision,
    consulted: Option<(&str, &Result<SelectorDecision, String>)>,
) -> CampaignDecision {
    let ordered = compute_selection(&snapshot.ready, &snapshot.open);
    let skipped = skipped_records(snapshot);
    if let Some(provider_choice) = match &consulted {
        // A well-formed, eligible proposal wins over the deterministic first
        // item (spec `30-model-routing.md` §9.45): resolve it back to its ready item.
        Some((_, Ok(selection))) => ordered
            .iter()
            .find(|item| task_id_for(&item.repo, item.number) == selection.task_id),
        _ => None,
    }
    .or_else(|| ordered.first())
    {
        let task_id = task_id_for(&provider_choice.repo, provider_choice.number);
        let (precedence_path, rationale) = selection_attribution(
            &ordered,
            &task_id,
            routing,
            consulted.as_ref().map(|(provider_id, result)| {
                (
                    *provider_id,
                    result.as_ref().map_err(std::string::String::as_str),
                )
            }),
        );
        let alternatives = ordered
            .iter()
            .filter(|item| task_id_for(&item.repo, item.number) != task_id)
            .map(|item| {
                let facts = facts_for(snapshot, &item.repo, item.number);
                BlockedNode {
                    repo: item.repo.clone(),
                    number: item.number,
                    title: item.title.clone(),
                    open_blockers: facts.map_or(0, |facts| facts.open_blockers),
                    target: facts.and_then(|facts| facts.target.clone()),
                    status: facts.and_then(|facts| facts.status.clone()),
                }
            })
            .collect();
        CampaignDecision {
            kind: DecisionKind::Selected,
            no_op_reason: None,
            selected: Some(SelectedTask {
                repo: provider_choice.repo.clone(),
                number: provider_choice.number,
                title: provider_choice.title.clone(),
                url: provider_choice.url.clone(),
                item_node_id: provider_choice.item_id.clone(),
                task_id: task_id.clone(),
            }),
            role: routing.role.clone(),
            provider: routing.provider.clone(),
            model: routing.model.clone(),
            precedence_path,
            fallback_reason: routing.fallback_reason.clone(),
            worker_args: worker_args(&task_id),
            rationale,
            alternatives,
            blocked_graph: Vec::new(),
            skipped,
        }
    } else {
        let (reason, rationale) = no_op_classification(snapshot);
        CampaignDecision {
            kind: DecisionKind::NoOp,
            no_op_reason: Some(reason),
            selected: None,
            role: routing.role.clone(),
            provider: routing.provider.clone(),
            model: routing.model.clone(),
            precedence_path: routing.precedence_path.clone(),
            fallback_reason: routing.fallback_reason.clone(),
            worker_args: Vec::new(),
            rationale,
            alternatives: Vec::new(),
            blocked_graph: blocked_candidate_nodes(snapshot),
            skipped,
        }
    }
}

/// Consults only when the ready queue is non-empty — a no-op pass has
/// nothing to select. Synchronous form for the one-shot CLI path.
fn campaign_decision_sync(
    snapshot: &BoardSnapshot,
    routing: &RoleRoutingDecision,
    selector: Option<&dyn DecisionProvider>,
) -> CampaignDecision {
    let ordered = compute_selection(&snapshot.ready, &snapshot.open);
    let consulted = selector.filter(|_| !ordered.is_empty()).map(|selector| {
        let ready_task_ids: Vec<String> = ordered
            .iter()
            .map(|item| task_id_for(&item.repo, item.number))
            .collect();
        (
            selector.id().as_str(),
            consult_task_selection(selector, &ready_task_ids),
        )
    });
    campaign_decision(
        snapshot,
        routing,
        consulted
            .as_ref()
            .map(|(provider_id, result)| (*provider_id, result)),
    )
}

/// Async form for the loop runner: the consultation is awaited (bounded by
/// [`DECISION_CONSULT_TIMEOUT`]) so supervision and the shutdown race keep
/// running while the endpoint answers.
fn campaign_decision_async<'a>(
    snapshot: &'a BoardSnapshot,
    routing: &'a RoleRoutingDecision,
    selector: Option<&'a dyn DecisionProvider>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = CampaignDecision> + Send + 'a>> {
    Box::pin(async move {
        let ordered = compute_selection(&snapshot.ready, &snapshot.open);
        let consulted = match selector.filter(|_| !ordered.is_empty()) {
            None => None,
            Some(selector) => {
                let ready_task_ids: Vec<String> = ordered
                    .iter()
                    .map(|item| task_id_for(&item.repo, item.number))
                    .collect();
                let provider_id = selector.id().as_str();
                let result = consult_task_selection_async(selector, &ready_task_ids).await;
                Some((provider_id, result))
            }
        };
        campaign_decision(
            snapshot,
            routing,
            consulted
                .as_ref()
                .map(|(provider_id, result)| (*provider_id, result)),
        )
    })
}

/// Computes the `precedence_path` and `rationale` a Selected record carries:
/// the decision-provider attribution when a configured selector produced an
/// eligible selection, otherwise the documented fallback cause (provider
/// error, out-of-set proposal, or no selector) over the deterministic
/// P0-first selection (spec `30-model-routing.md` §9.45).
fn selection_attribution(
    ordered: &[ReadyItem],
    task_id: &str,
    routing: &RoleRoutingDecision,
    consulted: Option<(&str, Result<&SelectorDecision, &str>)>,
) -> (String, String) {
    match consulted {
        Some((provider_id, Ok(selection))) => {
            if ordered
                .iter()
                .any(|item| task_id_for(&item.repo, item.number) == selection.task_id)
            {
                let confidence = selection
                    .confidence
                    .map_or_else(String::new, |value| format!(" (confidence {value:.2})"));
                return (
                    format!("decision-provider:{provider_id}{confidence}"),
                    format!(
                        "decision provider selected '{task_id}' among the eligible \
                         ready items (spec 30-model-routing.md §9.45)"
                    ),
                );
            }
            (
                routing.precedence_path.clone(),
                format!(
                    "decision provider '{provider_id}' proposed '{}' outside the \
                     eligible ready set; fell back to the first eligible item in P0-first \
                     ready order (spec 30-model-routing.md §9.45)",
                    selection.task_id,
                ),
            )
        }
        Some((provider_id, Err(reason))) => (
            routing.precedence_path.clone(),
            format!(
                "decision provider '{provider_id}' unavailable ({reason}); fell back \
                 to the first eligible item in P0-first ready order (spec \
                 30-model-routing.md §9.45)"
            ),
        ),
        None => (
            routing.precedence_path.clone(),
            "first eligible item in P0-first ready order (spec 10-orchestrator.md §9.35)"
                .to_string(),
        ),
    }
}

/// Runs exactly one pass: reads the snapshot, selects at most one task,
/// persists exactly one decision record, and — for a selection — spawns the
/// worker. No-op passes spawn nothing and still persist their typed record.
///
/// # Errors
///
/// Returns [`CampaignError::Store`] when the record cannot be persisted and
/// [`CampaignError::Spawn`] when the spawner fails to produce a run at all.
pub fn run_once(
    snapshot: &BoardSnapshot,
    routing: &RoleRoutingDecision,
    store: &CampaignDecisionStore,
    spawner: &dyn WorkerSpawner,
) -> Result<CampaignOutcome, CampaignError> {
    run_once_with_selector(snapshot, routing, store, spawner, None)
}

/// [`run_once`] with an optional configured decision provider (spec
/// `30-model-routing.md` §9.45): `Some` consults the provider for task
/// selection first; any provider error falls back to the deterministic
/// selector and is recorded in the decision record.
///
/// # Errors
///
/// Returns [`CampaignError::Store`] when the record cannot be persisted and
/// [`CampaignError::Spawn`] when the spawner fails to produce a run at all.
pub fn run_once_with_selector(
    snapshot: &BoardSnapshot,
    routing: &RoleRoutingDecision,
    store: &CampaignDecisionStore,
    spawner: &dyn WorkerSpawner,
    selector: Option<&dyn DecisionProvider>,
) -> Result<CampaignOutcome, CampaignError> {
    let stored = plan_pass_with_selector(snapshot, routing, store, selector)?;
    let worker = match &stored.decision.selected {
        Some(selected) => Some(spawner.spawn(&selected.task_id, routing)?),
        None => None,
    };
    Ok(CampaignOutcome {
        decision: stored,
        worker,
    })
}

fn worker_args(task_id: &str) -> Vec<String> {
    vec![
        "worker".to_string(),
        "run".to_string(),
        "--task".to_string(),
        task_id.to_string(),
        "--json".to_string(),
    ]
}

fn facts_for<'a>(snapshot: &'a BoardSnapshot, repo: &str, number: u64) -> Option<&'a ItemFacts> {
    snapshot
        .open
        .iter()
        .find(|facts| facts.repo == repo && facts.number == number)
}

fn skipped_records(snapshot: &BoardSnapshot) -> Vec<SkipRecord> {
    let mut records: Vec<SkipRecord> = snapshot
        .warnings
        .iter()
        .map(|warning| SkipRecord {
            number: warning.number,
            kind: match warning.kind {
                WarningKind::NotOpen => "not-open".to_string(),
                WarningKind::Malformed => "malformed".to_string(),
                WarningKind::Truncated => "truncated".to_string(),
            },
            reason: warning.reason.clone(),
        })
        .collect();
    records.sort_by(|left, right| (left.number, &left.kind).cmp(&(right.number, &right.kind)));
    records
}

fn blocked_candidate_nodes(snapshot: &BoardSnapshot) -> Vec<BlockedNode> {
    snapshot
        .blocked_candidates
        .iter()
        .map(|item| {
            let facts = facts_for(snapshot, &item.repo, item.number);
            BlockedNode {
                repo: item.repo.clone(),
                number: item.number,
                title: item.title.clone(),
                open_blockers: facts.map_or(0, |facts| facts.open_blockers),
                target: facts.and_then(|facts| facts.target.clone()),
                status: facts.and_then(|facts| facts.status.clone()),
            }
        })
        .collect()
}

/// Spec §9.35 discrimination: `all-blocked` only when eligible work exists
/// and every candidate is blocked; open items that merely are not Ready (no
/// blocked candidates) are `empty-queue` — "no eligible work exists at all".
/// `epic-exhausted` requires EVERY unevaluable item to be known-closed: a
/// malformed or truncated item's open-state is unknown, so claiming "every
/// tracked item is closed" would be fail-open.
fn no_op_classification(snapshot: &BoardSnapshot) -> (NoOpReason, String) {
    let note = warning_note(&snapshot.warnings);
    if snapshot.open.is_empty() {
        let all_known_closed = !snapshot.warnings.is_empty()
            && snapshot
                .warnings
                .iter()
                .all(|warning| warning.kind == WarningKind::NotOpen);
        if all_known_closed {
            return (
                NoOpReason::EpicExhausted,
                format!("every tracked item is closed; nothing open remains{note}"),
            );
        }
        return (
            NoOpReason::EmptyQueue,
            format!("no open board items in the configured repositories{note}"),
        );
    }
    if snapshot.blocked_candidates.is_empty() {
        return (
            NoOpReason::EmptyQueue,
            format!(
                "{} open item(s), none eligible under the ready predicate and none blocked{}",
                snapshot.open.len(),
                note
            ),
        );
    }
    (
        NoOpReason::AllBlocked,
        format!(
            "{} eligible candidate(s) blocked by unresolved dependencies; blocked graph attached{}",
            snapshot.blocked_candidates.len(),
            note
        ),
    )
}

/// Renders the warning channel into the rationale note shared by the no-op
/// branches: known-closed items are "recorded as closed"; any
/// malformed/truncated read turns the channel into a fail-closed disclosure.
fn warning_note(warnings: &[SkipWarning]) -> String {
    if warnings.is_empty() {
        return String::new();
    }
    let all_known_closed = warnings
        .iter()
        .all(|warning| warning.kind == WarningKind::NotOpen);
    if all_known_closed {
        format!("; {} item(s) recorded as closed", warnings.len())
    } else {
        format!(
            "; {} item(s) skipped as unevaluable (fail-closed)",
            warnings.len()
        )
    }
}
