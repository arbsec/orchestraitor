//! The §9.47(b) CI completions conditional poll lane: check-run and
//! workflow-run completions for head SHAs the daemon actively tracks.
//!
//! The repository events feed carries no CI completions (no
//! `CheckRunEvent`, no `WorkflowRunEvent`), so this lane runs a second
//! conditional poll over two ETag-capable endpoints:
//!
//! - `GET /repos/{owner}/{repo}/actions/runs?event=pull_request` (one
//!   per repository) — workflow-run completions AND the tracked head-SHA
//!   source: every in-flight (non-`completed`) `pull_request`-triggered
//!   run carries the head SHA the daemon is actively waiting on. The
//!   daemon holds no durable per-PR head-SHA state (run rows and PR
//!   handles carry identifiers, not SHAs), so the lane derives its own
//!   targets from the same response that reports run completions — no
//!   extra endpoint, no extra state, cost proportional to active work.
//! - `GET /repos/{owner}/{repo}/commits/{sha}/check-runs` (one per
//!   tracked head SHA) — check-run completions for the SHAs the runs
//!   poll discovered. The tracked set is the union of the SHAs of
//!   in-flight runs and the previously tracked SHAs (a SHA whose last
//!   run just completed may still have check runs completing after it);
//!   a SHA leaves the tracked set — and its per-SHA bookkeeping row is
//!   pruned — when the cap evicts it. The lane's first poll per process
//!   bypasses the persisted `runs` `ETag` (the tracked set is in-memory; a
//!   304 on the very first poll would hide active completions).
//!
//! Every request is conditional (`If-None-Match` with the stored
//! `ETag`); a `304` costs no quota and produces no events. The lane
//! honors `X-Poll-Interval` and rate-limit feedback exactly like the
//! events lane (§9.43: the cadence never widens beyond the feedback).
//! Events dedup on the durable delivery key (check-run id + conclusion
//! / run id + attempt + conclusion in the platform-delivery-id slot,
//! plus repository, normalized event type, and head SHA) with the same
//! 30-day redelivery window, and ride the same bounded ingress queue —
//! a redelivery never produces a second kick-off (§9.26.3). Only
//! completions are events: an in-flight run or pending check run adds
//! nothing to kick-off evaluation. Payloads carry identifiers only
//! (run/check-run id, repository, event type, number, head SHA,
//! conclusion) — never log or check output content. The §9.43 reconcile
//! poll stays the completeness authority: CI state the lane misses
//! arrives on the next reconcile pass.

use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;

use orchestraitor_board::BoardAuth;
use parking_lot::Mutex;
use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderValue, IF_NONE_MATCH, USER_AGENT};
use secrecy::ExposeSecret;
use serde::Deserialize;

use crate::error::DaemonError;
use crate::ingress::poller::{
    API_VERSION, CONNECT_TIMEOUT, DEFAULT_POLL_AFTER_SECS, MAX_RESPONSE_BYTES, PollError,
    PollOutcome, RATE_LIMIT_FALLBACK_SECS,
};
use crate::ingress::queue::IngressQueue;
use crate::ingress::store::{IngressEvent, IngressStore};

/// The connect timeout for one poll request (matches the events lane).
const TOTAL_TIMEOUT: std::time::Duration = std::time::Duration::from_mins(1);

/// The maximum number of head SHAs tracked per repository per round
/// (bounded so a flood of in-flight PR runs cannot multiply the lane's
/// request volume unboundedly — §7.3 discipline applied to the lane's
/// own fan-out). Oldest-discovered SHAs beyond the cap are dropped.
pub const MAX_TRACKED_HEAD_SHAS: usize = 16;

/// The `per_page` the lane pins on both endpoints (one page covers a
/// repository's active CI; pagination is deliberately not followed —
/// completions older than the first page are reconcile-poll territory,
/// and the events lane's bounded-pages rationale applies).
const PER_PAGE: &str = "100";

/// The normalized event type for a completed check run (the dedup
/// `event_type`).
pub const CHECK_RUN_COMPLETED: &str = "check-run-completed";

/// The normalized event type for a completed workflow run.
pub const WORKFLOW_RUN_COMPLETED: &str = "workflow-run-completed";

/// One repository-scoped CI lane target: the Actions-runs endpoint plus
/// the base URL the per-SHA check-runs endpoints hang off.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CiRepoTarget {
    /// The repository (`owner/name`).
    pub repo: String,
    /// The conditional GET URL for the pull_request-triggered runs.
    pub runs_url: String,
    /// The ETag-store key for the runs endpoint.
    pub runs_key: String,
    /// The API base the per-SHA check-runs URLs are built from.
    pub base_url: String,
}

/// Builds the CI lane targets for the configured repositories: one
/// Actions-runs endpoint per repository.
#[must_use]
pub fn ci_targets(repos: &[String], base_url: &str) -> Vec<CiRepoTarget> {
    let base = base_url.trim_end_matches('/');
    repos
        .iter()
        .map(|repo| CiRepoTarget {
            repo: repo.clone(),
            runs_url: format!(
                "{base}/repos/{repo}/actions/runs?event=pull_request&per_page={PER_PAGE}"
            ),
            runs_key: format!("{repo}/actions-runs"),
            base_url: base.to_string(),
        })
        .collect()
}

/// The subset of the workflow-run shape the lane consumes.
#[derive(Debug, Deserialize)]
struct WorkflowRunItem {
    #[serde(default)]
    id: Option<serde_json::Value>,
    /// The run's current status (`queued`, `in_progress`, `completed`).
    #[serde(default)]
    status: Option<String>,
    /// Set once the run completed (`success`, `failure`, ...).
    #[serde(default)]
    conclusion: Option<String>,
    /// The triggering event; the lane filters to `pull_request`.
    #[serde(default)]
    event: Option<String>,
    /// The head SHA the run built.
    #[serde(default)]
    head_sha: Option<String>,
    /// The run attempt number (the push-path delivery id includes it).
    #[serde(default)]
    run_attempt: Option<u64>,
    /// Associated PRs; fork PRs return an empty array (skipped).
    #[serde(default)]
    pull_requests: Vec<PullRef>,
}

/// The subset of the check-run shape the lane consumes.
#[derive(Debug, Deserialize)]
struct CheckRunItem {
    #[serde(default)]
    id: Option<serde_json::Value>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    conclusion: Option<String>,
    #[serde(default)]
    pull_requests: Vec<PullRef>,
}

#[derive(Debug, Deserialize)]
struct PullRef {
    #[serde(default)]
    number: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct RunsPage {
    #[serde(default)]
    workflow_runs: Vec<WorkflowRunItem>,
}

/// One derived CI event — the shared pipeline item (filter, shape, and
/// delivery key derived ONCE per item, never diverging between the
/// dedup check and the queue push).
#[derive(Debug, Clone, PartialEq, Eq)]
struct DerivedCiEvent {
    delivery_key: String,
    event_type: String,
    head_sha: Option<String>,
    number: u64,
}

/// Extracts a scalar delivery id from the JSON `id` field; a missing,
/// empty, or non-scalar id yields `None` (the item is skipped, never
/// folded into a shared empty key — the events lane's rule).
fn value_to_id(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::Number(number) => Some(number.to_string()),
        serde_json::Value::String(text) if !text.is_empty() => Some(text.clone()),
        _ => None,
    }
}

/// The first associated PR number, when the platform supplied one.
/// Fork-PR runs carry an empty `pull_requests` array — such items are
/// skipped (they are not this daemon's tracked agent PRs).
fn first_pr_number(pull_requests: &[PullRef]) -> Option<u64> {
    pull_requests.iter().find_map(|reference| reference.number)
}

/// Derives the event for a completed workflow run (`None` when the item
/// is filtered out: not completed, not `pull_request`-triggered, or
/// unshaped). The delivery-id slot carries the run id + attempt +
/// conclusion — a re-run (new attempt) or a conclusion transition is a
/// NEW delivery; the transition is exactly the signal.
fn workflow_run_event(item: &WorkflowRunItem, target: &CiRepoTarget) -> Option<DerivedCiEvent> {
    if item.status.as_deref() != Some("completed") {
        return None; // in-flight runs are not events
    }
    if item.event.as_deref() != Some("pull_request") {
        return None; // the lane tracks pull_request-triggered CI only
    }
    let id = item.id.as_ref().and_then(value_to_id)?;
    let attempt = item.run_attempt.unwrap_or(1);
    let conclusion = item.conclusion.as_deref().unwrap_or("unknown");
    let head_sha = item.head_sha.clone().filter(|sha| !sha.is_empty());
    let number = first_pr_number(&item.pull_requests)?;
    let delivery_id = format!("{id}:{attempt}:{conclusion}");
    Some(DerivedCiEvent {
        delivery_key: IngressStore::delivery_key(
            &delivery_id,
            &target.repo,
            WORKFLOW_RUN_COMPLETED,
            head_sha.as_deref(),
        ),
        event_type: WORKFLOW_RUN_COMPLETED.to_string(),
        head_sha: head_sha.clone(),
        number,
    })
}

/// Derives the event for a completed check run (`None` when filtered).
/// The delivery-id slot carries the check-run id + conclusion: a
/// status/conclusion transition of the same check run is a NEW delivery
/// — the transition is exactly the signal.
fn check_run_event(
    item: &CheckRunItem,
    target: &CiRepoTarget,
    head_sha: &str,
) -> Option<DerivedCiEvent> {
    if item.status.as_deref() != Some("completed") {
        return None; // pending / in-progress check runs are not events
    }
    let id = item.id.as_ref().and_then(value_to_id)?;
    let conclusion = item.conclusion.as_deref().unwrap_or("unknown");
    let number = first_pr_number(&item.pull_requests)?;
    let delivery_id = format!("{id}:{conclusion}");
    Some(DerivedCiEvent {
        delivery_key: IngressStore::delivery_key(
            &delivery_id,
            &target.repo,
            CHECK_RUN_COMPLETED,
            Some(head_sha),
        ),
        event_type: CHECK_RUN_COMPLETED.to_string(),
        head_sha: Some(head_sha.to_string()),
        number,
    })
}

/// Whether a workflow-run status means the run is still building
/// (anything but the terminal `completed`).
fn is_in_flight(status: Option<&str>) -> bool {
    status != Some("completed")
}

/// The §9.47(b) CI completions lane poller. Auth, the ETag/dedup store,
/// and the bounded queue are SHARED with the events lane (one ingress
/// queue, one dedup table, one rule set); the tracked head-SHA sets are
/// the lane's own in-memory bookkeeping (a restart re-discovers them
/// from the first runs response).
pub struct CiLane {
    auth: Arc<dyn BoardAuth>,
    http: reqwest::Client,
    store: Arc<std::sync::Mutex<IngressStore>>,
    queue: Arc<IngressQueue>,
    /// Repository runs endpoints this process has completed its first
    /// (ETag-bypassing) runs fetch for — after that, the persisted `ETag`
    /// rides normally.
    runs_initialized: Mutex<HashSet<String>>,
    /// Per-repository tracked head SHAs: the union of the in-flight run
    /// SHAs from the last runs response and the previously tracked SHAs
    /// (kept until the cap evicts them).
    tracked: Mutex<HashMap<String, HashSet<String>>>,
}

impl CiLane {
    /// Builds the lane over the given auth, shared ETag/dedup store, and
    /// shared bounded queue.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::IngressHttp`] when the HTTP client cannot
    /// be constructed.
    pub fn new(
        auth: Arc<dyn BoardAuth>,
        store: Arc<std::sync::Mutex<IngressStore>>,
        queue: Arc<IngressQueue>,
    ) -> Result<Self, DaemonError> {
        let http = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(TOTAL_TIMEOUT)
            .build()
            .map_err(|error| DaemonError::IngressHttp(format!("client build failed: {error}")))?;
        Ok(Self {
            auth,
            http,
            store,
            queue,
            runs_initialized: Mutex::new(HashSet::new()),
            tracked: Mutex::new(HashMap::new()),
        })
    }

    /// The minimum wait before the next lane round: the largest
    /// server-advised interval across the lane's endpoints (rate-limit
    /// feedback included). Callers sleep at least this long.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::IngressStore`] on store failure.
    pub fn next_poll_wait(
        &self,
        targets: &[CiRepoTarget],
    ) -> Result<std::time::Duration, DaemonError> {
        let store = self.lock_store()?;
        let mut max_secs = 0_u64;
        for target in targets {
            max_secs = max_secs.max(store.poll_after_secs(&target.runs_key)?);
        }
        Ok(std::time::Duration::from_secs(max_secs))
    }

    /// Polls one repository's CI lane once: the conditional runs poll
    /// (completions + head-SHA discovery), then one conditional
    /// check-runs poll per tracked head SHA. Each endpoint's outcome is
    /// independent — one failure never blocks the others (the reconcile
    /// poll keeps completeness regardless).
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::WatchConfig`] when the store mutex is
    /// poisoned (an internal defect must surface).
    pub async fn poll_repo(
        &self,
        target: &CiRepoTarget,
        now_secs: u64,
    ) -> Result<Vec<Result<PollOutcome, PollError>>, DaemonError> {
        let mut outcomes = Vec::new();
        // The previously tracked SHAs, captured BEFORE the runs poll
        // replaces the set: on a runs-poll failure the old SHAs keep
        // their check-runs polls (their completion signals must not
        // stall because one endpoint blipped).
        let previous = self.take_tracked(target);
        // First-success gate (a CodeRabbit review fix): the durable runs
        // ETag survives a daemon restart, but the in-memory tracked set
        // does not. Sending the persisted ETag on this process's FIRST
        // runs poll could yield a 304 with an empty tracked set — active
        // completions would stay undetected until the endpoint changes.
        // The first poll of this process therefore bypasses the persisted
        // ETag (a 200 is the tracked-set source of truth); afterwards the
        // ETag rides normally. `insert` returns true when the key was new
        // (this IS the first poll) — bypass exactly then.
        let bypass_etag = self.runs_initialized.lock().insert(target.runs_key.clone());
        match self
            .poll_runs(
                target,
                previous.iter().cloned().collect(),
                bypass_etag,
                now_secs,
            )
            .await
        {
            Ok(outcome) => outcomes.push(Ok(outcome)),
            Err(error) => {
                outcomes.push(Err(error));
                self.set_tracked(target, previous.iter().cloned().collect());
            }
        }
        let tracked_after_runs = self.take_tracked(target);
        for head_sha in &tracked_after_runs {
            outcomes.push(self.poll_check_runs(target, head_sha, now_secs).await);
        }
        // Prune per-SHA bookkeeping for SHAs the lane no longer tracks.
        // The prefix is repository-scoped: `check-runs|<repo>/` matches
        // only THIS repository's rows — a cross-repo prefix would delete
        // another repository's baseline rows and force its next check-runs
        // poll into the first-enable baseline (its completions would
        // never emit).
        let keep: Vec<String> = tracked_after_runs
            .iter()
            .map(|sha| check_runs_key(&target.repo, sha))
            .collect();
        let prune_prefix = format!("check-runs|{}/", target.repo);
        self.lock_store()?.prune_endpoints(&prune_prefix, &keep)?;
        Ok(outcomes)
    }

    /// The conditional runs poll: 304 (zero quota, no events), 200
    /// (workflow-run completions into dedup + queue; in-flight SHAs
    /// become the tracked set), rate-limit and failure arms typed for
    /// cadence handling.
    async fn poll_runs(
        &self,
        target: &CiRepoTarget,
        previous_tracked: HashSet<String>,
        bypass_etag: bool,
        now_secs: u64,
    ) -> Result<PollOutcome, PollError> {
        let etag = if bypass_etag {
            None
        } else {
            self.lock_store()
                .map_err(|_| PollError::Failed)?
                .etag_for(&target.runs_key)
                .map_err(|_| PollError::Failed)?
        };
        let response = self
            .send_conditional(&target.runs_url, etag.as_deref())
            .await?;
        let status = response.status();
        let poll_after = poll_interval_of(response.headers());
        if status.as_u16() == 304 {
            self.record_conditional(&target.runs_key, etag.as_deref(), poll_after, true)
                .map_err(|_| PollError::Failed)?;
            return Ok(PollOutcome {
                accepted: 0,
                duplicates: 0,
                dropped: 0,
                poll_after_secs: poll_after,
            });
        }
        if let Some(wait) = rate_limit_wait(response.headers(), status, now_secs) {
            self.record_conditional(&target.runs_key, etag.as_deref(), wait, false)
                .map_err(|_| PollError::Failed)?;
            return Err(PollError::RateLimited {
                retry_after_secs: wait,
            });
        }
        if !status.is_success() {
            self.record_conditional(&target.runs_key, etag.as_deref(), poll_after, false)
                .map_err(|_| PollError::Failed)?;
            return Err(PollError::Failed);
        }
        let new_etag = etag_of(response.headers());
        let bytes = read_bounded(response).await?;
        let page: RunsPage = serde_json::from_slice(&bytes).map_err(|_| PollError::Failed)?;
        // Head-SHA discovery: every in-flight pull_request-triggered run
        // carries a head SHA the daemon is actively waiting on. Deduped
        // BEFORE the cap (repeated in-flight runs of one SHA must not
        // spend the budget) and bounded: the newest MAX_TRACKED_HEAD_SHAS
        // distinct SHAs win (the response is newest-first), so a flood of
        // PR runs cannot multiply the lane's fan-out unboundedly.
        let mut seen_shas = HashSet::new();
        let in_flight: Vec<String> = page
            .workflow_runs
            .iter()
            .filter(|run| is_in_flight(run.status.as_deref()))
            .filter(|run| run.event.as_deref() == Some("pull_request"))
            .filter_map(|run| run.head_sha.clone().filter(|sha| !sha.is_empty()))
            .filter(|sha| seen_shas.insert(sha.clone()))
            .take(MAX_TRACKED_HEAD_SHAS)
            .collect();
        let derived: Vec<DerivedCiEvent> = page
            .workflow_runs
            .iter()
            .filter_map(|run| workflow_run_event(run, target))
            .collect();
        let outcome = self.process_events(&derived, now_secs)?;
        self.record_conditional(&target.runs_key, new_etag.as_deref(), poll_after, false)
            .map_err(|_| PollError::Failed)?;
        // The tracked set is the UNION of the previously tracked SHAs and
        // the newly in-flight SHAs: a SHA whose last workflow run just
        // completed still has check runs that may complete after the run
        // (third-party checks included) — it stays tracked until its
        // check-runs endpoint answers 304 quiet. The cap applies to the
        // union (newest-first: in-flight SHAs first, then previously
        // tracked), so a flood cannot grow the fan-out unboundedly.
        let mut union = in_flight.clone();
        for sha in previous_tracked {
            if !union.contains(&sha) {
                union.push(sha);
            }
        }
        union.truncate(MAX_TRACKED_HEAD_SHAS);
        self.set_tracked(target, union);
        Ok(outcome)
    }

    /// The conditional check-runs poll for one tracked head SHA. The
    /// first 200 baselines (registers current completions, emits
    /// nothing — pre-existing check runs are history); later 200s emit
    /// newly-completed check runs; a 304 after the SHA's runs completed
    /// marks the SHA quiet (the caller prunes it).
    async fn poll_check_runs(
        &self,
        target: &CiRepoTarget,
        head_sha: &str,
        now_secs: u64,
    ) -> Result<PollOutcome, PollError> {
        let key = check_runs_key(&target.repo, head_sha);
        let url = format!(
            "{}/repos/{}/commits/{}/check-runs?per_page={PER_PAGE}",
            target.base_url, target.repo, head_sha
        );
        let etag = self
            .lock_store()
            .map_err(|_| PollError::Failed)?
            .etag_for(&key)
            .map_err(|_| PollError::Failed)?;
        let response = self.send_conditional(&url, etag.as_deref()).await?;
        let status = response.status();
        let poll_after = poll_interval_of(response.headers());
        if status.as_u16() == 304 {
            self.record_conditional(&key, etag.as_deref(), poll_after, true)
                .map_err(|_| PollError::Failed)?;
            // Quiet after completion: the store's not_modified marker is
            // the durable record; the caller drops the SHA when its runs
            // are all completed (the tracked set no longer holds it).
            return Ok(PollOutcome {
                accepted: 0,
                duplicates: 0,
                dropped: 0,
                poll_after_secs: poll_after,
            });
        }
        if let Some(wait) = rate_limit_wait(response.headers(), status, now_secs) {
            self.record_conditional(&key, etag.as_deref(), wait, false)
                .map_err(|_| PollError::Failed)?;
            return Err(PollError::RateLimited {
                retry_after_secs: wait,
            });
        }
        if !status.is_success() {
            self.record_conditional(&key, etag.as_deref(), poll_after, false)
                .map_err(|_| PollError::Failed)?;
            return Err(PollError::Failed);
        }
        let new_etag = etag_of(response.headers());
        let bytes = read_bounded(response).await?;
        let page: CheckRunPage = serde_json::from_slice(&bytes).map_err(|_| PollError::Failed)?;
        let derived: Vec<DerivedCiEvent> = page
            .check_runs
            .iter()
            .filter_map(|run| check_run_event(run, target, head_sha))
            .collect();
        let first_enable = !self
            .lock_store()
            .map_err(|_| PollError::Failed)?
            .is_baselined(&key)
            .map_err(|_| PollError::Failed)?;
        if first_enable {
            // First-enable baseline: register the current completions as
            // already-seen (history from before the SHA was tracked), the
            // events queue untouched — the same posture as the events
            // lane's baseline (a spurious flood of stale kick-offs on
            // first sight would defeat the acceleration purpose).
            {
                let mut store = self.lock_store().map_err(|_| PollError::Failed)?;
                for event in &derived {
                    store
                        .record_delivery(&event.delivery_key, now_secs)
                        .map_err(|_| PollError::Failed)?;
                }
                store.mark_baselined(&key).map_err(|_| PollError::Failed)?;
            }
            self.record_conditional(&key, new_etag.as_deref(), poll_after, false)
                .map_err(|_| PollError::Failed)?;
            return Ok(PollOutcome {
                accepted: 0,
                duplicates: 0,
                dropped: 0,
                poll_after_secs: poll_after,
            });
        }
        let outcome = self.process_events(&derived, now_secs)?;
        self.record_conditional(&key, new_etag.as_deref(), poll_after, false)
            .map_err(|_| PollError::Failed)?;
        Ok(outcome)
    }

    /// Processes derived events through durable dedup into the bounded
    /// queue (the events lane's discipline: dedup BEFORE the queue, a
    /// redelivery never consumes capacity).
    fn process_events(
        &self,
        events: &[DerivedCiEvent],
        now_secs: u64,
    ) -> Result<PollOutcome, PollError> {
        let mut accepted = 0_usize;
        let mut duplicates = 0_usize;
        let mut dropped = 0_usize;
        for event in events {
            let is_new = self
                .lock_store()
                .map_err(|_| PollError::Failed)?
                .record_delivery(&event.delivery_key, now_secs)
                .map_err(|_| PollError::Failed)?;
            if !is_new {
                duplicates += 1;
                continue;
            }
            let ingress_event = IngressEvent {
                delivery_key: event.delivery_key.clone(),
                repo: event
                    .delivery_key
                    .split('|')
                    .nth(1)
                    .unwrap_or_default()
                    .to_string(),
                event_type: event.event_type.clone(),
                head_sha: event.head_sha.clone(),
                number: event.number,
                accepted_at_secs: now_secs,
            };
            match self.queue.push(ingress_event, now_secs) {
                crate::ingress::queue::PushOutcome::Accepted => accepted += 1,
                crate::ingress::queue::PushOutcome::Duplicate => duplicates += 1,
                crate::ingress::queue::PushOutcome::Dropped => dropped += 1,
            }
        }
        Ok(PollOutcome {
            accepted,
            duplicates,
            dropped,
            poll_after_secs: 0,
        })
    }

    /// Sends one conditional GET with the lane's pinned headers.
    async fn send_conditional(
        &self,
        url: &str,
        etag: Option<&str>,
    ) -> Result<reqwest::Response, PollError> {
        let token = self
            .auth
            .bearer_token()
            .await
            .map_err(|_| PollError::Failed)?;
        let mut request = self
            .http
            .get(url)
            .header(AUTHORIZATION, format!("Bearer {}", token.expose_secret()))
            .header(
                USER_AGENT,
                HeaderValue::from_static("orchestraitor-ingress"),
            )
            .header(
                ACCEPT,
                HeaderValue::from_static("application/vnd.github+json"),
            )
            .header(
                "X-GitHub-Api-Version",
                HeaderValue::from_static(API_VERSION),
            );
        if let Some(etag) = etag {
            request = request.header(
                IF_NONE_MATCH,
                HeaderValue::from_str(etag).map_err(|_| PollError::Failed)?,
            );
        }
        request.send().await.map_err(|_| PollError::Failed)
    }

    fn record_conditional(
        &self,
        key: &str,
        etag: Option<&str>,
        poll_after_secs: u64,
        not_modified: bool,
    ) -> Result<(), DaemonError> {
        self.lock_store()?
            .record_conditional(key, etag, poll_after_secs, not_modified)
    }

    fn take_tracked(&self, target: &CiRepoTarget) -> HashSet<String> {
        self.tracked
            .lock()
            .get(&target.repo)
            .cloned()
            .unwrap_or_default()
    }

    fn set_tracked(&self, target: &CiRepoTarget, shas: Vec<String>) {
        self.tracked
            .lock()
            .insert(target.repo.clone(), shas.into_iter().collect());
    }

    fn lock_store(&self) -> Result<std::sync::MutexGuard<'_, IngressStore>, DaemonError> {
        self.store.lock().map_err(|_| DaemonError::IngressStore {
            path: "<shared>".to_string(),
            reason: "ingress store mutex poisoned".to_string(),
        })
    }
}

/// The ETag-store key for one head SHA's check-runs endpoint.
fn check_runs_key(repo: &str, head_sha: &str) -> String {
    format!("check-runs|{repo}/{head_sha}")
}

#[derive(Debug, Deserialize)]
struct CheckRunPage {
    #[serde(default)]
    check_runs: Vec<CheckRunItem>,
}

/// The server-advised minimum wait (`X-Poll-Interval`, seconds); the
/// documented default when absent (the same courtesy as the events lane).
fn poll_interval_of(headers: &reqwest::header::HeaderMap) -> u64 {
    headers
        .get("X-Poll-Interval")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(DEFAULT_POLL_AFTER_SECS)
}

/// The response `ETag`, when present.
fn etag_of(headers: &reqwest::header::HeaderMap) -> Option<String> {
    headers
        .get(reqwest::header::ETAG)
        .and_then(|value| value.to_str().ok())
        .map(std::string::ToString::to_string)
}

/// The typed rate-limit decision (§9.43): `Some(wait)` when the status
/// is a rate limit (429 always; 403 only with a zero-remaining or
/// `Retry-After` header — a plain 403 is permission failure, never
/// "rate-limited"). `Retry-After` wins, else wait exactly until the
/// reset epoch second (bounded below by the platform default so a bad
/// clock cannot spin), else the bounded fallback.
fn rate_limit_wait(
    headers: &reqwest::header::HeaderMap,
    status: reqwest::StatusCode,
    now_secs: u64,
) -> Option<u64> {
    let is_limit = status.as_u16() == 429
        || (status.as_u16() == 403
            && (headers
                .get("X-RateLimit-Remaining")
                .and_then(|value| value.to_str().ok())
                == Some("0")
                || headers.contains_key("Retry-After")));
    if !is_limit {
        return None;
    }
    let retry_after = headers
        .get("Retry-After")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    let reset = headers
        .get("X-RateLimit-Reset")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    Some(match (retry_after, reset) {
        (Some(after), _) => after,
        (None, Some(reset_epoch)) => reset_epoch
            .saturating_sub(now_secs)
            .max(DEFAULT_POLL_AFTER_SECS),
        _ => RATE_LIMIT_FALLBACK_SECS,
    })
}

/// Reads the response body under the shared byte bound (a larger body
/// is refused fail-closed rather than trusted).
async fn read_bounded(response: reqwest::Response) -> Result<Vec<u8>, PollError> {
    if let Some(length) = response.content_length()
        && length > MAX_RESPONSE_BYTES
    {
        return Err(PollError::Failed);
    }
    let body = response.bytes().await.map_err(|_| PollError::Failed)?;
    if body.len() as u64 > MAX_RESPONSE_BYTES {
        return Err(PollError::Failed);
    }
    Ok(body.to_vec())
}
