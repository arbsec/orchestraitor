//! The §9.50(b) conditional-request poller (issue #504): the poll-only
//! event ingress over the GitHub REST API.
//!
//! Every poll sends `If-None-Match` with the endpoint's stored `ETag`; a
//! `304 Not Modified` costs nothing and produces no events, a `200`
//! carries the changed event list. The server's `X-Poll-Interval` is
//! honored as the minimum wait before the next poll of that endpoint,
//! and rate-limit feedback (§9.43: `X-RateLimit-Remaining: 0` /
//! `Retry-After` / HTTP 403-429) adapts the cadence — the next poll
//! never fires before the limit resets, and the poller never widens its
//! cadence beyond that feedback (spec §9.50(b): "never automatic
//! widening of the poll cadence beyond rate-limit feedback").
//!
//! Accepted events flow through the durable dedup table into the bounded
//! queue; the §9.43 reconcile poll stays the completeness authority, so
//! a missed or delayed event never loses work.

use std::sync::Arc;
use std::time::Duration;

use orchestraitor_board::BoardAuth;
use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderValue, IF_NONE_MATCH, LINK, USER_AGENT};
use secrecy::ExposeSecret;

use crate::error::DaemonError;
use crate::ingress::queue::{IngressQueue, PushOutcome};
use crate::ingress::store::{IngressEvent, IngressStore};

/// The GitHub API version the poller pins (same as the board client).
pub(crate) const API_VERSION: &str = "2022-11-28";

/// The connect timeout for one poll request (matches the board client).
pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// The total timeout for one poll request (matches the board client).
const TOTAL_TIMEOUT: Duration = Duration::from_mins(1);

/// The server-advised minimum wait when a response carries no
/// `X-Poll-Interval` (the platform's documented default cadence).
pub const DEFAULT_POLL_AFTER_SECS: u64 = 60;

/// The hard backoff when a rate limit is hit without a usable reset
/// hint (the same courtesy the loop's backoff applies — bounded, never
/// unbounded retry).
pub const RATE_LIMIT_FALLBACK_SECS: u64 = 300;

/// The maximum accepted response body (matches the board client's
/// bound; a larger body is refused fail-closed rather than trusted).
pub(crate) const MAX_RESPONSE_BYTES: u64 = 8 * 1024 * 1024;

/// The maximum pagination pages followed per poll (bounded so a
/// hostile or looping `next` link cannot spin the poller; 10 pages at
/// up to 100 events each is far beyond any plausible poll delta).
const MAX_PAGES_PER_POLL: usize = 10;

/// Whether `next` carries the same scheme, host, and effective port as
/// `base` (the bearer token must never leave the configured origin).
fn same_origin(next: &str, base: &str) -> bool {
    match (reqwest::Url::parse(next), reqwest::Url::parse(base)) {
        (Ok(n), Ok(b)) => {
            n.scheme() == b.scheme()
                && n.host_str() == b.host_str()
                && n.port_or_known_default() == b.port_or_known_default()
        }
        _ => false,
    }
}

/// Parses the `rel="next"` URL out of an RFC 5987 `Link` header value.
fn next_link(link_header: &str) -> Option<String> {
    for part in link_header.split(',') {
        let mut segments = part.split(';');
        let Some(url_part) = segments.next() else {
            continue;
        };
        let url = url_part
            .trim()
            .trim_start_matches('<')
            .trim_end_matches('>');
        let mut is_next = false;
        for param in segments {
            let param = param.trim();
            if let Some(rel) = param.strip_prefix("rel=")
                && rel.trim_matches('"') == "next"
            {
                is_next = true;
            }
        }
        if is_next {
            return Some(url.to_string());
        }
    }
    None
}

/// The event types the poll fallback tracks (spec §9.50: new issues,
/// issue/PR comments, PR ready-for-review). The operator's
/// `[watch.ingress].enabled` gate selects this fixed set; the names are
/// the normalized dedup `event_type` values. NOTE (gen-19): the
/// repository events feed does not carry `ready_for_review` (its
/// `PullRequestEvent` action set lacks it), so `pr-ready-for-review` is
/// temporarily REMOVED from the filter — it returns when the poller
/// gains a supported source for it (the issue-events endpoint).
pub const EVENT_TYPES: [&str; 3] = ["issues-opened", "issue-commented", "pr-commented"];

/// One completed poll request over one endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollOutcome {
    /// How many events passed dedup and were enqueued.
    pub accepted: usize,
    /// How many were redeliveries inside the window (deduplicated).
    pub duplicates: usize,
    /// How many were dropped (queue at capacity; `degraded` recorded).
    pub dropped: usize,
    /// The minimum wait before the NEXT poll of this endpoint (the
    /// honored `X-Poll-Interval`, widened ONLY by rate-limit feedback —
    /// never past the limit's reset).
    pub poll_after_secs: u64,
}

/// A poll failure for one endpoint, typed for cadence handling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PollError {
    /// The endpoint was rate-limited; `retry_after_secs` is the minimum
    /// wait before the next attempt (the reset hint when present, else
    /// the bounded fallback).
    RateLimited {
        /// Minimum wait before retrying, in seconds.
        retry_after_secs: u64,
    },
    /// Any other transport/HTTP/shape failure; the caller backs off on
    /// the bounded default cadence (a failed poll never widens the
    /// cadence and never retries in a tight loop).
    Failed,
}

/// One repository-scoped poll target: one conditional request per
/// repository per collection — GitHub's REST event feeds are
/// repo-scoped (`/repos/{owner}/{name}/{collection}/events`), so the
/// dedup key and the event's repo field carry exactly one repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollTarget {
    /// Absolute endpoint URL (for tests, a fixture server base).
    pub url: String,
    /// The ETag-store key for this endpoint.
    pub key: String,
    /// The single repository this endpoint serves (`owner/name`).
    pub repo: String,
}

/// Builds the poll targets for the configured repositories: one
/// repository events endpoint per repository (`GET
/// /repos/{owner}/{repo}/events` — the documented feed, which supports
/// `ETag`/304 and `X-Poll-Interval`; issue and PR activity both appear
/// there under the `{type, payload}` shape). One conditional request
/// per repository per poll round: sharing one URL across repositories
/// would mislabel every event, and per-repo scoping keeps the dedup
/// key and the event's repo field exact.
#[must_use]
pub fn poll_targets(repos: &[String], base_url: &str) -> Vec<PollTarget> {
    let base = base_url.trim_end_matches('/');
    repos
        .iter()
        .map(|repo| PollTarget {
            url: format!("{base}/repos/{repo}/events"),
            key: format!("{repo}/events"),
            repo: repo.clone(),
        })
        .collect()
}

/// One fixture payload item (the subset of the REST event shape the
/// poller consumes); parsed from the response array.
#[derive(Debug, serde::Deserialize)]
struct RestEventItem {
    #[serde(default)]
    id: Option<serde_json::Value>,
    /// The repository-event type (`IssuesEvent`, `IssueCommentEvent`,
    /// `PullRequestEvent`, ...).
    #[serde(rename = "type")]
    kind: String,
    /// The acting principal (§9.50(c) launch fact; §9.52(a) author-class
    /// derivation input). Absent on malformed items — the derived event
    /// then carries no author and the intake attach fails closed.
    #[serde(default)]
    actor: Option<RestActor>,
    #[serde(default)]
    payload: Option<RestPayload>,
}

/// The event's actor: the launch-fact author identity.
#[derive(Debug, serde::Deserialize)]
struct RestActor {
    #[serde(default)]
    login: Option<String>,
}

/// The author (issue/PR creator) when the payload carries one: for issue
/// events the ACTOR is the author, but the payload's `user` is the
/// authoritative author identity, so both are read and the payload wins
/// when present.
#[derive(Debug, serde::Deserialize)]
struct RestUser {
    #[serde(default)]
    login: Option<String>,
    /// The platform's user type (`"User"`, `"Bot"`, ...): the launch
    /// fact the §9.52(a) bot classification reads.
    #[serde(rename = "type", default)]
    user_type: Option<String>,
}

/// The `payload` object; which fields carry the number depends on the
/// event kind (issues carry `issue`, PR activity carries
/// `pull_request`).
#[derive(Debug, serde::Deserialize)]
struct RestPayload {
    /// The activity verb (`opened`, `closed`, `synchronize`,
    /// `ready_for_review`, ...). Only the documented §9.50 verbs pass
    /// the filter — every `IssuesEvent`/`IssueCommentEvent`/
    /// `PullRequestEvent` type carries multiple actions whose delivery
    /// keys (head SHA included) would otherwise multiply kick-offs
    /// (a gen-13 cloud review finding).
    #[serde(default)]
    action: Option<String>,
    #[serde(default)]
    issue: Option<RestIssueRef>,
    #[serde(default)]
    pull_request: Option<RestPullRef>,
}

#[derive(Debug, serde::Deserialize)]
struct RestIssueRef {
    number: u64,
    #[serde(default)]
    pull_request: Option<serde_json::Value>,
    /// The issue author (§9.52(a) launch fact).
    #[serde(default)]
    user: Option<RestUser>,
}

#[derive(Debug, serde::Deserialize)]
struct RestPullRef {
    number: u64,
    #[serde(default)]
    head: Option<RestPullHead>,
    /// The PR author (§9.52(a) launch fact).
    #[serde(default)]
    user: Option<RestUser>,
}

#[derive(Debug, serde::Deserialize)]
struct RestPullHead {
    #[serde(default)]
    sha: Option<String>,
}

/// Normalizes a repository-event `type` into the operator-facing event
/// type (the dedup `event_type`). Untracked kinds return `None`
/// (filtered out before dedup — the operator's event filter is the
/// fixed §9.50 set). On the repository events feed, PRs appear as
/// issues carrying `payload.issue.pull_request`; `IssueCommentEvent`
/// distinguishes PR comments from issue comments that way.
fn normalize_event_type(
    kind: &str,
    issue_marks_pull: bool,
    action: Option<&str>,
) -> Option<String> {
    match (kind, action, issue_marks_pull) {
        ("IssuesEvent", Some("opened"), false) => Some("issues-opened".to_string()),
        ("IssueCommentEvent", Some("created"), false) => Some("issue-commented".to_string()),
        ("IssueCommentEvent", Some("created"), true) => Some("pr-commented".to_string()),
        _ => None,
    }
}

/// The §9.50(b) conditional-request poller. Auth rides the board
/// provider's [`BoardAuth`] seam (public/board-visible events, the same
/// credentials the reconcile poll uses); `ETag` bookkeeping rides the
/// durable [`IngressStore`]; accepted events ride the bounded
/// [`IngressQueue`].
pub struct EventPoller {
    auth: Arc<dyn BoardAuth>,
    http: reqwest::Client,
    store: Arc<std::sync::Mutex<IngressStore>>,
    queue: Arc<IngressQueue>,
    /// The §9.52(a) intake store (leaf #664): every accepted
    /// `issues-opened` event is ALSO written here (durable), so the
    /// intake attach lane consumes from its own durable surface — the
    /// kick-off source's consumption of the shared queue can never
    /// steal an attach, and a queue-surplus drop is not a lost attach.
    /// `None` when the lane is disabled (no intake writes).
    intake_store: Option<Arc<dyn crate::ingress::intake::IntakeStore>>,
}

impl EventPoller {
    /// Builds the poller over the given auth, ETag/dedup store, and
    /// bounded queue (no intake fan-out — the lane disabled).
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
        Self::with_intake(auth, store, queue, None)
    }

    /// Builds the poller with an optional §9.52(a) intake fan-out
    /// store. `Some(store)` mirrors every accepted `issues-opened`
    /// event into the durable intake surface (leaf #664).
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::IngressHttp`] when the HTTP client cannot
    /// be constructed.
    pub fn with_intake(
        auth: Arc<dyn BoardAuth>,
        store: Arc<std::sync::Mutex<IngressStore>>,
        queue: Arc<IngressQueue>,
        intake_store: Option<Arc<dyn crate::ingress::intake::IntakeStore>>,
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
            intake_store,
        })
    }

    /// Polls every target once, in order. Each endpoint is a conditional
    /// request; per-endpoint outcomes are independent (one endpoint's
    /// failure never blocks the others — the reconcile poll keeps
    /// completeness regardless).
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::WatchConfig`] when the store mutex is
    /// poisoned (an internal defect must surface).
    pub async fn poll_all(
        &self,
        targets: &[PollTarget],
        now_secs: u64,
    ) -> Result<Vec<Result<PollOutcome, PollError>>, DaemonError> {
        let mut outcomes = Vec::with_capacity(targets.len());
        for target in targets {
            outcomes.push(self.poll_one(target, now_secs).await);
        }
        Ok(outcomes)
    }

    /// The minimum wait before the next poll round: the largest
    /// server-advised interval across the endpoints (rate-limit feedback
    /// included). Callers sleep at least this long; the cadence never
    /// widens beyond this feedback.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::IngressStore`] on store failure.
    pub fn next_poll_wait(&self, targets: &[PollTarget]) -> Result<Duration, DaemonError> {
        let store = self.lock_store()?;
        let mut max_secs = 0_u64;
        for target in targets {
            max_secs = max_secs.max(store.poll_after_secs(&target.key)?);
        }
        Ok(Duration::from_secs(max_secs))
    }

    /// Polls one endpoint with its stored `ETag`.
    #[expect(
        clippy::too_many_lines,
        reason = "the poll flow (conditional request, 304/429/auth-error arms, bounded pagination chain, dedup+queue) reads top-to-bottom as one sequence; extracting arms would scatter the single response contract"
    )]
    async fn poll_one(&self, target: &PollTarget, now_secs: u64) -> Result<PollOutcome, PollError> {
        let etag = self
            .lock_store()
            .map_err(|_| PollError::Failed)?
            .etag_for(&target.key)
            .map_err(|_| PollError::Failed)?;
        let token = self
            .auth
            .bearer_token()
            .await
            .map_err(|_| PollError::Failed)?;
        let mut request = self
            .http
            .get(&target.url)
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
        if let Some(etag) = etag.as_deref() {
            request = request.header(
                IF_NONE_MATCH,
                HeaderValue::from_str(etag).map_err(|_| PollError::Failed)?,
            );
        }
        let response = request.send().await.map_err(|_| PollError::Failed)?;
        let status = response.status();
        let poll_interval = response
            .headers()
            .get("X-Poll-Interval")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok());
        let poll_after = poll_interval.unwrap_or(DEFAULT_POLL_AFTER_SECS);
        if status.as_u16() == 304 {
            self.record_conditional(&target.key, etag.as_deref(), poll_after, true)
                .map_err(|_| PollError::Failed)?;
            return Ok(PollOutcome {
                accepted: 0,
                duplicates: 0,
                dropped: 0,
                poll_after_secs: poll_after,
            });
        }
        // 429 is always a rate limit; a 403 only when GitHub's
        // rate-limit headers say so (a plain 403 is permission or auth
        // failure — surfacing it as "rate-limited" would mislead the
        // operator, a gen-5 review finding).
        let limited_403 = status.as_u16() == 403
            && (response
                .headers()
                .get("X-RateLimit-Remaining")
                .and_then(|v| v.to_str().ok())
                == Some("0")
                || response.headers().contains_key("Retry-After"));
        if status.as_u16() == 429 || limited_403 {
            let wait = Self::rate_limit_wait_secs(response.headers(), now_secs);
            self.record_conditional(&target.key, etag.as_deref(), wait, false)
                .map_err(|_| PollError::Failed)?;
            return Err(PollError::RateLimited {
                retry_after_secs: wait,
            });
        }
        if !status.is_success() {
            self.record_conditional(&target.key, etag.as_deref(), poll_after, false)
                .map_err(|_| PollError::Failed)?;
            return Err(PollError::Failed);
        }
        let new_etag = response
            .headers()
            .get(ETAG)
            .and_then(|value| value.to_str().ok())
            .map(std::string::ToString::to_string);
        // Follow the Link-header pagination chain (bounded): the event
        // feed is a paginated collection, and only the first page would
        // otherwise be processed (a gen-6 cloud review finding). The
        // chain stops at the FIRST page containing an already-seen
        // delivery key — the feed lists newest-first, so everything
        // after that page is history the dedup table already judged
        // (a gen-7 cloud review finding: unbounded historical paging
        // would re-accept history on first enable and fail when the
        // history exceeds the page cap). The page cap is a successful
        // partial poll, never an error: the §9.43 reconcile poll stays
        // the completeness authority for anything left.
        //
        // FIRST-ENABLE BASELINE (a gen-9 review finding): when no ETag
        // is stored, this poll is the operator's starting point — every
        // item on the pages is history from BEFORE the ingress existed.
        // Recording it would fill the queue with pre-enable activity and
        // arm kick-offs for work that predates the feature. Instead the
        // first poll only REGISTERS the page-1 keys in the dedup table
        // (so the next poll's redelivery window sees them as history)
        // and consumes the ETag — no event reaches the queue on the
        // very first 200.
        let first_enable = !self
            .lock_store()
            .map_err(|_| PollError::Failed)?
            .is_baselined(&target.key)
            .map_err(|_| PollError::Failed)?;
        let mut items: Vec<RestEventItem> = Vec::new();
        // The durable boundary: the maximum raw platform id already
        // processed for this endpoint. A page whose EVERY raw id is at
        // or below it is already-processed history — whatever the event
        // type (untracked types dominate busy feeds; keying the boundary
        // on tracked delivery keys alone never matched durable history,
        // a gen-17 review finding).
        let last_seen_raw_id = self
            .lock_store()
            .map_err(|_| PollError::Failed)?
            .max_raw_id(&target.key)
            .map_err(|_| PollError::Failed)?;
        // The boundary as of BEFORE this poll (the skip guard below uses
        // it; the store's max is updated only after processing).
        let max_raw_id_this_poll_before_update = last_seen_raw_id;
        let mut max_raw_id_this_poll = last_seen_raw_id;
        let mut page = Some(response);
        let mut pages = 0_usize;
        while let Some(current) = page {
            pages += 1;
            let link = current
                .headers()
                .get(LINK)
                .and_then(|value| value.to_str().ok())
                .map(std::string::ToString::to_string);
            let length = current.content_length();
            if let Some(length) = length
                && length > MAX_RESPONSE_BYTES
            {
                return Err(PollError::Failed);
            }
            let bytes = current.bytes().await.map_err(|_| PollError::Failed)?;
            if bytes.len() as u64 > MAX_RESPONSE_BYTES {
                return Err(PollError::Failed);
            }
            // Per-item tolerance (a gen-23 review finding): one item with
            // an unexpected payload shape must not block the endpoint's
            // ingress permanently (the poll would fail before the ETag/
            // boundary advance, and the next poll would reach the same
            // item). Parse the page as raw values, convert each item
            // individually, and skip the failures; the boundary scan
            // still counts a skipped item's raw id.
            let raw_items: Vec<serde_json::Value> =
                serde_json::from_slice(&bytes).map_err(|_| PollError::Failed)?;
            let mut page_items: Vec<RestEventItem> = raw_items
                .into_iter()
                .filter_map(|raw| serde_json::from_value(raw).ok())
                .collect();
            // Boundary detection uses EVERY item's raw platform id, not
            // only tracked ones: an all-untracked page must still end
            // the chain once it reaches already-processed history (the
            // feed lists newest-first). A page cap hit is a successful
            // partial poll, never an error — the §9.43 reconcile poll
            // stays the completeness authority for anything left.
            let page_reached_boundary = !page_items.is_empty()
                && page_items.iter().all(|item| {
                    item.id
                        .as_ref()
                        .and_then(value_to_id)
                        .and_then(|id| id.parse::<u64>().ok())
                        .is_some_and(|id| last_seen_raw_id.is_some_and(|last| id <= last))
                });
            for raw_id in page_items.iter().filter_map(|item| {
                item.id
                    .as_ref()
                    .and_then(value_to_id)
                    .and_then(|id| id.parse::<u64>().ok())
            }) {
                max_raw_id_this_poll =
                    max_raw_id_this_poll.map_or(Some(raw_id), |last| Some(last.max(raw_id)));
            }
            items.append(&mut page_items);
            if page_reached_boundary {
                break;
            }
            if pages >= MAX_PAGES_PER_POLL {
                break; // successful partial poll; never an error
            }
            page = match link.as_deref().and_then(next_link) {
                // Same-origin only: a `Link` header can point anywhere;
                // the bearer token must never go to another host or a
                // downgrade to plain HTTP (a gen-15 review finding).
                Some(next) if !next.is_empty() && same_origin(&next, &target.url) => {
                    let token = self
                        .auth
                        .bearer_token()
                        .await
                        .map_err(|_| PollError::Failed)?;
                    let next_response = self
                        .http
                        .get(next)
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
                        )
                        .send()
                        .await
                        .map_err(|_| PollError::Failed)?;
                    if !next_response.status().is_success() {
                        return Err(PollError::Failed);
                    }
                    Some(next_response)
                }
                _ => None,
            };
        }
        if first_enable {
            // Baseline registration only: record every page-1 delivery
            // key as already-seen, then persist the ETag. The queue is
            // untouched; the next poll (with the ETag stored) processes
            // only the delta against this baseline.
            let mut store = self.lock_store().map_err(|_| PollError::Failed)?;
            for key in items
                .iter()
                .filter_map(|item| item_derived(item, target).map(|derived| derived.delivery_key))
            {
                // Propagated: a failed registration would leave the key
                // unrecorded, so the next delta poll would enqueue it as
                // a spurious new event (a gen-15 review finding).
                store
                    .record_delivery(&key, now_secs)
                    .map_err(|_| PollError::Failed)?;
            }
            // The raw-id boundary covers EVERY item (tracked or not), so
            // history beyond the baseline never re-enters the chain (a
            // gen-19 review finding: without it, the second poll's
            // boundary is unset and the full page cap is walked).
            for raw_id in items.iter().filter_map(|item| {
                item.id
                    .as_ref()
                    .and_then(value_to_id)
                    .and_then(|id| id.parse::<u64>().ok())
            }) {
                store
                    .record_max_raw_id(&target.key, raw_id)
                    .map_err(|_| PollError::Failed)?;
            }
            store
                .mark_baselined(&target.key)
                .map_err(|_| PollError::Failed)?;
            drop(store);
            self.record_conditional(&target.key, new_etag.as_deref(), poll_after, false)
                .map_err(|_| PollError::Failed)?;
            return Ok(PollOutcome {
                accepted: 0,
                duplicates: 0,
                dropped: 0,
                poll_after_secs: poll_after,
            });
        }
        let (accepted, duplicates, dropped) = self
            .process_items(&items, target, now_secs, max_raw_id_this_poll_before_update)
            .await?;
        if let Some(max_id) = max_raw_id_this_poll {
            self.lock_store()
                .map_err(|_| PollError::Failed)?
                .record_max_raw_id(&target.key, max_id)
                .map_err(|_| PollError::Failed)?;
        }
        self.record_conditional(&target.key, new_etag.as_deref(), poll_after, false)
            .map_err(|_| PollError::Failed)?;
        Ok(PollOutcome {
            accepted,
            duplicates,
            dropped,
            poll_after_secs: poll_after,
        })
    }

    /// Processes one endpoint's event items through dedup and the bounded
    /// queue; returns (accepted, duplicates, dropped) counts.
    async fn process_items(
        &self,
        items: &[RestEventItem],
        target: &PollTarget,
        now_secs: u64,
        last_seen_raw_id: Option<u64>,
    ) -> Result<(usize, usize, usize), PollError> {
        let mut accepted = 0_usize;
        let mut duplicates = 0_usize;
        let mut dropped = 0_usize;
        for item in items {
            // The full derivation (filter, shape, action) is shared with
            // the pagination boundary via `item_derived` — one copy, so
            // the stop check and the dedup table can never diverge (a
            // gen-17 review finding).
            let Some(derived) = item_derived(item, target) else {
                continue; // outside the operator's §9.50 event filter or unshaped
            };
            // Items at or below the raw-id boundary are already-processed
            // history — skipped BEFORE the dedup check (a gen-18 review
            // finding: the feed retains up to 90 days but the dedup table
            // only 30, so a mixed page's old items need the raw-id guard,
            // not just the window).
            if let Some(raw_id) = item
                .id
                .as_ref()
                .and_then(value_to_id)
                .and_then(|id| id.parse::<u64>().ok())
                && last_seen_raw_id.is_some_and(|last| raw_id <= last)
            {
                continue;
            }
            // Dedup is DURABLE (the store), before the queue: a redelivery
            // never consumes queue capacity and never produces a second
            // kick-off (§9.26.3 via §9.50).
            let is_new = self
                .lock_store()
                .map_err(|_| PollError::Failed)?
                .record_delivery(&derived.delivery_key, now_secs)
                .map_err(|_| PollError::Failed)?;
            if !is_new {
                duplicates += 1;
                continue;
            }
            let event = IngressEvent {
                delivery_key: derived.delivery_key,
                repo: target.repo.clone(),
                event_type: derived.event_type,
                head_sha: derived.head_sha,
                number: derived.number,
                author_login: derived.author_login,
                author_is_bot: derived.author_is_bot,
                accepted_at_secs: now_secs,
            };
            // The §9.52(a) intake fan-out (leaf #664): mirror every
            // accepted `issues-opened` event into the DURABLE intake
            // store BEFORE the queue push. A store write failure counts
            // as a drop (the event stays out of the lane's surface; the
            // kick-off path is untouched) — never a silent miss.
            if event.event_type == "issues-opened"
                && let Some(intake) = &self.intake_store
                && intake.put(&event).await.is_err()
            {
                dropped += 1;
                continue;
            }
            match self.queue.push(event, now_secs) {
                PushOutcome::Accepted => accepted += 1,
                PushOutcome::Duplicate => duplicates += 1, // unreachable via this path; kept for exhaustiveness
                PushOutcome::Dropped => dropped += 1,
            }
        }
        Ok((accepted, duplicates, dropped))
    }

    /// The rate-limit wait (§9.43 feedback, §9.50(b): the cadence never
    /// widens beyond the feedback): an explicit `Retry-After` wins, else
    /// wait exactly until the reset epoch second (bounded below by the
    /// platform default so a bad clock cannot spin), else the bounded
    /// fallback.
    fn rate_limit_wait_secs(headers: &reqwest::header::HeaderMap, now_secs: u64) -> u64 {
        let retry_after = headers
            .get("Retry-After")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok());
        let reset = headers
            .get("X-RateLimit-Reset")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok());
        match (retry_after, reset) {
            (Some(after), _) => after,
            (None, Some(reset_epoch)) => reset_epoch
                .saturating_sub(now_secs)
                .max(DEFAULT_POLL_AFTER_SECS),
            _ => RATE_LIMIT_FALLBACK_SECS,
        }
    }

    /// Persists one endpoint's conditional bookkeeping.
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

    /// Locks the ETag/dedup store.
    fn lock_store(&self) -> Result<std::sync::MutexGuard<'_, IngressStore>, DaemonError> {
        self.store.lock().map_err(|_| DaemonError::IngressStore {
            path: "<shared>".to_string(),
            reason: "ingress store mutex poisoned".to_string(),
        })
    }
}

use reqwest::header::ETAG;

/// Everything the pipeline needs from one item — derived ONCE and shared
/// by the pagination boundary and `process_items` (a gen-17 review finding:
/// duplicate derivations can diverge, deduplicating an event under a
/// different key than the stop check judged).
#[derive(Debug, Clone, PartialEq, Eq)]
struct DerivedItem {
    /// The durable delivery key (id + repo + event type + head SHA).
    delivery_key: String,
    /// The normalized event type.
    event_type: String,
    /// The head SHA for PR events, when present.
    head_sha: Option<String>,
    /// The issue or PR number the event is about.
    number: u64,
    /// The author login (§9.50(c) launch fact; `None` = the platform
    /// supplied none and the intake attach fails closed).
    author_login: Option<String>,
    /// Whether the platform declared the author a bot.
    author_is_bot: Option<bool>,
}

/// Derives [`DerivedItem`] (None when the item would be filtered or
/// skipped later — it never enters the seen set or the queue).
fn item_derived(item: &RestEventItem, target: &PollTarget) -> Option<DerivedItem> {
    let is_pull = item.payload.as_ref().is_some_and(|payload| {
        payload.pull_request.is_some()
            || payload
                .issue
                .as_ref()
                .is_some_and(|issue| issue.pull_request.is_some())
    });
    let action = item
        .payload
        .as_ref()
        .and_then(|payload| payload.action.as_deref());
    let event_type = normalize_event_type(&item.kind, is_pull, action)?;
    let delivery_id = item.id.as_ref().and_then(value_to_id)?;
    let head_sha = if is_pull {
        item.payload
            .as_ref()
            .and_then(|payload| payload.pull_request.as_ref())
            .and_then(|pr| pr.head.as_ref())
            .and_then(|head| head.sha.clone())
    } else {
        None
    };
    let number = if is_pull {
        item.payload
            .as_ref()
            .and_then(|payload| payload.pull_request.as_ref().map(|pr| pr.number))
            .or_else(|| {
                item.payload
                    .as_ref()
                    .and_then(|payload| payload.issue.as_ref().map(|issue| issue.number))
            })
    } else {
        item.payload
            .as_ref()
            .and_then(|payload| payload.issue.as_ref().map(|issue| issue.number))
    }?;
    // Author launch facts (§9.50(c), §9.52(a)): the payload's author
    // (`user`) is authoritative when present; the event's `actor` is the
    // fallback. The bot fact rides whichever identity won — reading the
    // actor's bot flag for a payload author would mix two principals.
    let (author_login, author_is_bot) = if is_pull {
        author_facts(
            item.payload
                .as_ref()
                .and_then(|payload| payload.pull_request.as_ref())
                .and_then(|pr| pr.user.as_ref()),
            item.actor.as_ref(),
        )
    } else {
        author_facts(
            item.payload
                .as_ref()
                .and_then(|payload| payload.issue.as_ref())
                .and_then(|issue| issue.user.as_ref()),
            item.actor.as_ref(),
        )
    };
    Some(DerivedItem {
        delivery_key: IngressStore::delivery_key(
            &delivery_id,
            &target.repo,
            &event_type,
            head_sha.as_deref(),
        ),
        event_type,
        head_sha,
        number,
        author_login,
        author_is_bot,
    })
}

/// Resolves the (`login`, `is_bot`) author launch facts: the payload
/// author wins over the event actor; a `Bot` user type is the platform's
/// bot declaration. `None` login = no usable launch fact (fail closed).
fn author_facts(
    payload_user: Option<&RestUser>,
    actor: Option<&RestActor>,
) -> (Option<String>, Option<bool>) {
    if let Some(user) = payload_user {
        let login = user.login.clone().filter(|login| !login.is_empty());
        let is_bot = user
            .user_type
            .as_deref()
            .map(|user_type| user_type == "Bot");
        return (login, is_bot);
    }
    if let Some(actor) = actor {
        let login = actor.login.clone().filter(|login| !login.is_empty());
        if login.is_some() {
            // The actor shape carries no user-type; the bot fact is
            // unknown, not false — the attach classifies conservatively.
            return (login, None);
        }
    }
    (None, None)
}

/// Extracts the delivery identifier from the REST item's `id` field
/// (number or non-empty string on the wire); a missing, empty, or
/// non-scalar id yields `None` — the item is skipped, never folded into
/// a shared empty key.
fn value_to_id(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::Number(number) => Some(number.to_string()),
        serde_json::Value::String(text) if !text.is_empty() => Some(text.clone()),
        _ => None,
    }
}
