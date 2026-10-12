//! The §9.47(b) conditional-request poller (issue #504): the poll-only
//! event ingress over the GitHub REST API.
//!
//! Every poll sends `If-None-Match` with the endpoint's stored `ETag`; a
//! `304 Not Modified` costs nothing and produces no events, a `200`
//! carries the changed event list. The server's `X-Poll-Interval` is
//! honored as the minimum wait before the next poll of that endpoint,
//! and rate-limit feedback (§9.43: `X-RateLimit-Remaining: 0` /
//! `Retry-After` / HTTP 403-429) adapts the cadence — the next poll
//! never fires before the limit resets, and the poller never widens its
//! cadence beyond that feedback (spec §9.47(b): "never automatic
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
const API_VERSION: &str = "2022-11-28";

/// The connect timeout for one poll request (matches the board client).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

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
const MAX_RESPONSE_BYTES: u64 = 8 * 1024 * 1024;

/// The maximum pagination pages followed per poll (bounded so a
/// hostile or looping `next` link cannot spin the poller; 10 pages at
/// up to 100 events each is far beyond any plausible poll delta).
const MAX_PAGES_PER_POLL: usize = 10;

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

/// The event types the poll fallback tracks (spec §9.47: new issues,
/// issue/PR comments, PR ready-for-review). The operator's
/// `[watch.ingress].enabled` gate selects this fixed set; the names are
/// the normalized dedup `event_type` values.
pub const EVENT_TYPES: [&str; 4] = [
    "issues-opened",
    "issue-commented",
    "pr-commented",
    "pr-ready-for-review",
];

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
/// repository-scoped issue-events endpoint per repository (`GET
/// /repos/{owner}/{repo}/issues/events` — the documented feed; GitHub
/// treats pull requests as issues, so PR events arrive on the same feed
/// identified by the item's `pull_request` property). One conditional
/// request per repository per poll round: sharing one URL across
/// repositories would mislabel every event, and per-repo scoping keeps
/// the dedup key and the event's repo field exact.
#[must_use]
pub fn poll_targets(repos: &[String], base_url: &str) -> Vec<PollTarget> {
    let base = base_url.trim_end_matches('/');
    repos
        .iter()
        .map(|repo| PollTarget {
            url: format!("{base}/repos/{repo}/issues/events"),
            key: format!("{repo}/issues-events"),
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
    event: String,
    #[serde(default)]
    issue: Option<RestIssueRef>,
    #[serde(default)]
    pull_request: Option<RestPullRef>,
}

#[derive(Debug, serde::Deserialize)]
struct RestIssueRef {
    number: u64,
}

#[derive(Debug, serde::Deserialize)]
struct RestPullRef {
    number: u64,
    #[serde(default)]
    head: Option<RestPullHead>,
}

#[derive(Debug, serde::Deserialize)]
struct RestPullHead {
    #[serde(default)]
    sha: Option<String>,
}

/// Normalizes a raw platform `event` verb into the operator-facing
/// event type (the dedup `event_type`). Untracked verbs return `None`
/// (filtered out before dedup — the operator's event filter is the
/// fixed §9.47 set).
fn normalize_event_type(raw: &str, is_pull: bool) -> Option<String> {
    match (raw, is_pull) {
        ("opened", false) => Some("issues-opened".to_string()),
        ("commented", false) => Some("issue-commented".to_string()),
        ("commented", true) => Some("pr-commented".to_string()),
        ("ready_for_review", true) => Some("pr-ready-for-review".to_string()),
        _ => None,
    }
}

/// The §9.47(b) conditional-request poller. Auth rides the board
/// provider's [`BoardAuth`] seam (public/board-visible events, the same
/// credentials the reconcile poll uses); `ETag` bookkeeping rides the
/// durable [`IngressStore`]; accepted events ride the bounded
/// [`IngressQueue`].
pub struct EventPoller {
    auth: Arc<dyn BoardAuth>,
    http: reqwest::Client,
    store: Arc<std::sync::Mutex<IngressStore>>,
    queue: Arc<IngressQueue>,
}

impl EventPoller {
    /// Builds the poller over the given auth, ETag/dedup store, and
    /// bounded queue.
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
        let first_enable = etag.is_none();
        let mut items: Vec<RestEventItem> = Vec::new();
        let mut seen_keys: std::collections::HashSet<String> = std::collections::HashSet::new();
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
            let mut page_items: Vec<RestEventItem> =
                serde_json::from_slice(&bytes).map_err(|_| PollError::Failed)?;
            // Stop when this page adds nothing new: its keys are all
            // already seen (newest-first ordering makes the page head
            // the newest entry, so a fully-seen page means the rest is
            // history).
            let page_keys: Vec<String> = page_items
                .iter()
                .filter_map(|item| item_delivery_key(item, target))
                .collect();
            // The stop condition consults the DURABLE dedup table (any
            // key recorded inside the redelivery window — including
            // from earlier polls), not just this poll's own pages: the
            // feed lists newest-first, so a page whose keys the table
            // already judged is the boundary into already-processed
            // history (a gen-8 cloud review finding — a per-poll key
            // set alone never matches durable history, so every 200
            // response would replay the full page cap). The guard is
            // scoped to the synchronous lookups; it must never span
            // the await below.
            let page_all_seen = {
                let store = self.lock_store().map_err(|_| PollError::Failed)?;
                let mut all_seen = !page_keys.is_empty();
                for key in &page_keys {
                    match store.delivery_seen_within_window(key, now_secs) {
                        Ok(true) => {}
                        Ok(false) => {
                            all_seen = false;
                            break;
                        }
                        Err(_) => return Err(PollError::Failed),
                    }
                }
                all_seen
            };
            seen_keys.extend(page_keys.iter().cloned());
            items.append(&mut page_items);
            if page_all_seen {
                break;
            }
            if pages >= MAX_PAGES_PER_POLL {
                break; // successful partial poll; never an error
            }
            page = match link.as_deref().and_then(next_link) {
                Some(next) if !next.is_empty() => {
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
                .filter_map(|item| item_delivery_key(item, target))
            {
                let _ignore = store.record_delivery(&key, now_secs);
            }
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
        let (accepted, duplicates, dropped) = self.process_items(&items, target, now_secs)?;
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
    fn process_items(
        &self,
        items: &[RestEventItem],
        target: &PollTarget,
        now_secs: u64,
    ) -> Result<(usize, usize, usize), PollError> {
        let mut accepted = 0_usize;
        let mut duplicates = 0_usize;
        let mut dropped = 0_usize;
        for item in items {
            let is_pull = item.pull_request.is_some();
            let Some(event_type) = normalize_event_type(&item.event, is_pull) else {
                continue; // outside the operator's §9.47 event filter
            };
            let number = if is_pull {
                item.pull_request.as_ref().map(|pr| pr.number)
            } else {
                item.issue.as_ref().map(|issue| issue.number)
            };
            let Some(number) = number else {
                continue; // unshaped payload: skip, never guess
            };
            let Some(delivery_id) = item.id.as_ref().and_then(value_to_id) else {
                continue; // no platform delivery id: skip, never share a key
            };
            let head_sha = if is_pull {
                item.pull_request
                    .as_ref()
                    .and_then(|pr| pr.head.as_ref())
                    .and_then(|head| head.sha.clone())
            } else {
                None
            };
            let delivery_key = IngressStore::delivery_key(
                &delivery_id,
                &target.repo,
                &event_type,
                head_sha.as_deref(),
            );
            // Dedup is DURABLE (the store), before the queue: a redelivery
            // never consumes queue capacity and never produces a second
            // kick-off (§9.26.3 via §9.47).
            let is_new = self
                .lock_store()
                .map_err(|_| PollError::Failed)?
                .record_delivery(&delivery_key, now_secs)
                .map_err(|_| PollError::Failed)?;
            if !is_new {
                duplicates += 1;
                continue;
            }
            let event = IngressEvent {
                delivery_key,
                repo: target.repo.clone(),
                event_type,
                head_sha,
                number,
                accepted_at_secs: now_secs,
            };
            match self.queue.push(event, now_secs) {
                PushOutcome::Accepted => accepted += 1,
                PushOutcome::Duplicate => duplicates += 1, // unreachable via this path; kept for exhaustiveness
                PushOutcome::Dropped => dropped += 1,
            }
        }
        Ok((accepted, duplicates, dropped))
    }

    /// The rate-limit wait (§9.43 feedback, §9.47(b): the cadence never
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

/// The item's full delivery key (None when the item would be filtered
/// or skipped later — it never enters the seen set).
fn item_delivery_key(item: &RestEventItem, target: &PollTarget) -> Option<String> {
    let is_pull = item.pull_request.is_some();
    let event_type = normalize_event_type(&item.event, is_pull)?;
    let delivery_id = item.id.as_ref().and_then(value_to_id)?;
    let head_sha = if is_pull {
        item.pull_request
            .as_ref()
            .and_then(|pr| pr.head.as_ref())
            .and_then(|head| head.sha.clone())
    } else {
        None
    };
    Some(IngressStore::delivery_key(
        &delivery_id,
        &target.repo,
        &event_type,
        head_sha.as_deref(),
    ))
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
