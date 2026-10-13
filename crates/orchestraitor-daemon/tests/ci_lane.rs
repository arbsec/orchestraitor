//! Fixture-driven integration tests for the §9.47(b) CI completions
//! conditional poll lane: the CI lane poller runs against the same
//! scripted in-process HTTP server pattern as the events lane (no live
//! network in CI). Negative and adversarial cases (§21.4) pin the
//! delivery-key/dedup contract, the first-enable baseline, and the
//! rate-limit backoff behavior.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
// Test-only allowances mirror the crate's other integration tests: a
// failed assertion must fail the test loudly.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use http_body_util::Full;
use hyper::body::{Bytes, Incoming};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use orchestraitor_board::BoardAuth;
use orchestraitor_daemon::ingress::ci_lane::{CiLane, CiRepoTarget, ci_targets};
use orchestraitor_daemon::ingress::poller::PollError;
use orchestraitor_daemon::ingress::queue::IngressQueue;
use orchestraitor_daemon::ingress::store::IngressStore;
use secrecy::SecretString;
use serde_json::json;

const TOKEN: &str = "fixture-token-value";

/// Injected auth returning the fixture token.
struct StaticAuth;

#[async_trait]
impl BoardAuth for StaticAuth {
    async fn bearer_token(&self) -> Result<SecretString, orchestraitor_board::BoardError> {
        Ok(SecretString::from(TOKEN.to_string()))
    }
}

/// One request the fixture server captured.
#[derive(Debug, Clone)]
struct Captured {
    if_none_match: Option<String>,
}

/// One scripted response rule.
struct Rule {
    /// Requests whose path contains this needle match the rule.
    path_needle: &'static str,
    /// `If-None-Match` value the rule requires (`None` = matches any).
    requires_etag: Option<String>,
    status: u16,
    body: String,
    headers: Vec<(&'static str, String)>,
}

/// A scripted HTTP fixture server on `127.0.0.1` (ephemeral port): the
/// same discipline as the events lane's server — each request consumes
/// the FIRST unconsumed matching rule; an unmatched request answers 500.
struct FixtureServer {
    base: String,
    captured: Arc<std::sync::Mutex<Vec<Captured>>>,
    shutdown: Arc<std::sync::atomic::AtomicBool>,
    join: Option<tokio::task::JoinHandle<()>>,
}

impl FixtureServer {
    async fn start(rules: Vec<Rule>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fixture server bind");
        let addr = listener.local_addr().expect("fixture server addr");
        let captured = Arc::new(std::sync::Mutex::new(Vec::new()));
        let shutdown = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let rules = Arc::new(std::sync::Mutex::new(rules));
        let captured_thread = Arc::clone(&captured);
        let shutdown_thread = Arc::clone(&shutdown);
        let join = tokio::spawn(async move {
            loop {
                if shutdown_thread.load(std::sync::atomic::Ordering::SeqCst) {
                    return;
                }
                let accepted =
                    tokio::time::timeout(Duration::from_millis(50), listener.accept()).await;
                let Ok(Ok((stream, _))) = accepted else {
                    continue;
                };
                let io = hyper_util::rt::TokioIo::new(stream);
                let rules_conn = Arc::clone(&rules);
                let captured_conn = Arc::clone(&captured_thread);
                let _result: Result<(), hyper::Error> = http1::Builder::new()
                    .serve_connection(
                        io,
                        service_fn(move |req: Request<Incoming>| {
                            let rules = Arc::clone(&rules_conn);
                            let captured = Arc::clone(&captured_conn);
                            let response = handle(&rules, &captured, req);
                            async move { Ok::<_, Infallible>(response) }
                        }),
                    )
                    .await;
            }
        });
        Self {
            base: format!("http://{addr}"),
            captured,
            shutdown,
            join: Some(join),
        }
    }

    fn requests(&self) -> Vec<Captured> {
        self.captured.lock().expect("captured lock").clone()
    }
}

impl Drop for FixtureServer {
    fn drop(&mut self) {
        self.shutdown
            .store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(join) = self.join.take() {
            join.abort();
        }
    }
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "hyper's service_fn hands the request owned; the handler reads it and lets it drop"
)]
fn handle(
    rules: &Arc<std::sync::Mutex<Vec<Rule>>>,
    captured: &Arc<std::sync::Mutex<Vec<Captured>>>,
    req: Request<Incoming>,
) -> Response<http_body_util::Full<Bytes>> {
    let if_none_match = req
        .headers()
        .get("if-none-match")
        .and_then(|value| value.to_str().ok())
        .map(std::string::ToString::to_string);
    let path = req.uri().path().to_string();
    captured.lock().expect("captured lock").push(Captured {
        if_none_match: if_none_match.clone(),
    });
    let matched = {
        let mut rules = rules.lock().expect("rules lock");
        let index = rules.iter().position(|rule| {
            path.contains(rule.path_needle)
                && match (&rule.requires_etag, &if_none_match) {
                    (None, _) => true,
                    (Some(required), Some(sent)) => required == sent,
                    (Some(_), None) => false,
                }
        });
        index.map(|index| rules.remove(index))
    };
    let Some(rule) = matched else {
        return Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .body(Full::new(Bytes::from("unscripted request")))
            .expect("static response");
    };
    let mut builder =
        Response::builder().status(StatusCode::from_u16(rule.status).expect("valid status"));
    for (name, value) in &rule.headers {
        builder = builder.header(*name, value);
    }
    builder
        .body(Full::new(Bytes::from(rule.body)))
        .expect("static response")
}

/// Builds a fresh queue + store + CI lane triple over in-memory state.
fn fixture_lane() -> (
    Arc<IngressQueue>,
    Arc<std::sync::Mutex<IngressStore>>,
    CiLane,
) {
    let queue = Arc::new(IngressQueue::new(8).expect("valid capacity"));
    let store = Arc::new(std::sync::Mutex::new(
        IngressStore::open_in_memory().expect("in-memory store"),
    ));
    let lane = CiLane::new(Arc::new(StaticAuth), Arc::clone(&store), Arc::clone(&queue))
        .expect("lane builds");
    (queue, store, lane)
}

/// The CI lane target pointing at this server (repo-scoped).
fn target_of(server: &FixtureServer) -> CiRepoTarget {
    ci_targets(&["arbsec/orchestraitor".to_string()], &server.base).remove(0)
}

/// A runs page: one completed run for PR 504 (head `cafe12`), one
/// in-flight run for PR 505 (head `beef34`) — the in-flight one is the
/// tracked-SHA source, the completed one is a completion event.
fn runs_body() -> String {
    json!({
        "workflow_runs": [
            {
                "id": 900,
                "status": "completed",
                "conclusion": "success",
                "event": "pull_request",
                "head_sha": "cafe12",
                "run_attempt": 1,
                "pull_requests": [ { "number": 504 } ]
            },
            {
                "id": 901,
                "status": "in_progress",
                "event": "pull_request",
                "head_sha": "beef34",
                "run_attempt": 1,
                "pull_requests": [ { "number": 505 } ]
            }
        ]
    })
    .to_string()
}

/// A check-runs page for head `beef34`: one completed check run.
fn check_runs_body(id: u64, conclusion: &str) -> String {
    json!({
        "check_runs": [
            {
                "id": id,
                "status": "completed",
                "conclusion": conclusion,
                "pull_requests": [ { "number": 505 } ]
            }
        ]
    })
    .to_string()
}

/// The delivery key for the fixture's completed workflow run.
fn run_key(conclusion: &str, attempt: u64) -> String {
    format!("900:{attempt}:{conclusion}|arbsec/orchestraitor|workflow-run-completed|cafe12")
}

/// AC: a completed workflow run becomes an event; an in-flight run does
/// NOT (only completions are kick-off signals) and its head SHA becomes
/// a tracked check-runs target (visible in the request log).
#[tokio::test(flavor = "current_thread")]
async fn completed_runs_are_events_in_flight_runs_are_tracked_targets() {
    let server = FixtureServer::start(vec![Rule {
        path_needle: "actions/runs",
        requires_etag: None,
        status: 200,
        body: runs_body(),
        headers: vec![("X-Poll-Interval", "60".to_string())],
    }])
    .await;
    let (queue, _store, lane) = fixture_lane();
    let target = target_of(&server);

    let mut outcomes = lane.poll_repo(&target, 1_000).await.expect("poll round");
    assert_eq!(
        outcomes.len(),
        2,
        "the runs poll plus the discovered in-flight SHA's check-runs poll"
    );
    let runs_outcome = outcomes.remove(0).expect("runs poll ok");
    assert_eq!(
        (runs_outcome.accepted, runs_outcome.duplicates),
        (1, 0),
        "exactly the completed run is accepted; the in-flight run is not an event"
    );
    // The check-runs poll for beef34 is the first-enable baseline: the
    // fixture server has no rule for it, so it answers 500 -> Failed
    // (an unscripted request is a test bug the server surfaces loudly).
    // This test scripts only the runs endpoint; the Failed arm proves
    // the SHA was tracked.
    assert!(
        matches!(outcomes.remove(0), Err(PollError::Failed)),
        "the in-flight run's head SHA became a tracked check-runs target (unscripted here)"
    );

    let drained = queue.take();
    assert_eq!(drained.len(), 1, "one completion event enqueued");
    assert_eq!(
        drained[0].delivery_key,
        run_key("success", 1),
        "the delivery key carries run id + attempt + conclusion + repo + type + head SHA"
    );
    assert_eq!(drained[0].event_type, "workflow-run-completed");
    assert_eq!(drained[0].number, 504);
    assert_eq!(drained[0].head_sha.as_deref(), Some("cafe12"));
}

/// AC (§21.4 negative): a repeated poll of the SAME run is a redelivery —
/// duplicates = 1, accepted = 0, the queue never receives a second copy.
#[tokio::test(flavor = "current_thread")]
async fn a_repoll_of_the_same_run_never_duplicates_the_delivery() {
    let server = FixtureServer::start(vec![
        Rule {
            path_needle: "actions/runs",
            requires_etag: None,
            status: 200,
            body: runs_body(),
            headers: vec![("ETag", "\"r1\"".to_string())],
        },
        Rule {
            path_needle: "actions/runs",
            requires_etag: Some("\"r1\"".to_string()),
            status: 200,
            body: runs_body(),
            headers: vec![("ETag", "\"r1\"".to_string())],
        },
    ])
    .await;
    let (queue, _store, lane) = fixture_lane();
    let target = target_of(&server);

    let first = lane
        .poll_repo(&target, 1_000)
        .await
        .expect("first round")
        .remove(0)
        .expect("runs poll ok");
    assert_eq!(first.accepted, 1, "the first poll accepts the completion");

    let second = lane
        .poll_repo(&target, 1_100)
        .await
        .expect("second round")
        .remove(0)
        .expect("runs poll ok");
    assert_eq!(
        (second.accepted, second.duplicates),
        (0, 1),
        "the repeated run is deduplicated: zero accepted, one duplicate"
    );
    assert_eq!(
        queue.peek().len(),
        1,
        "the queue never receives a second copy of the same delivery"
    );
}

/// AC: a check-run status/conclusion transition (`pending` -> `success`)
/// is a NEW delivery under the durable key; a re-poll of the unchanged
/// check run is deduplicated. The SHA's FIRST check-runs poll baselines
/// (registers, emits nothing) — the transition fires on the second.
#[tokio::test(flavor = "current_thread")]
async fn check_run_transition_is_a_new_delivery_and_repoll_dedups() {
    let server = FixtureServer::start(vec![
        Rule {
            path_needle: "actions/runs",
            requires_etag: None,
            status: 200,
            body: runs_body(),
            headers: vec![("X-Poll-Interval", "60".to_string())],
        },
        // Baseline: in-flight run for beef34 -> check-runs poll for it.
        Rule {
            path_needle: "commits/beef34/check-runs",
            requires_etag: None,
            status: 200,
            body: check_runs_body(700, "success"),
            headers: vec![("ETag", "\"c1\"".to_string())],
        },
        Rule {
            path_needle: "actions/runs",
            requires_etag: None,
            status: 200,
            body: runs_body(),
            headers: vec![("X-Poll-Interval", "60".to_string())],
        },
        Rule {
            path_needle: "commits/beef34/check-runs",
            requires_etag: Some("\"c1\"".to_string()),
            status: 200,
            body: check_runs_body(700, "failure"),
            headers: vec![("ETag", "\"c2\"".to_string())],
        },
        Rule {
            path_needle: "actions/runs",
            requires_etag: None,
            status: 200,
            body: runs_body(),
            headers: vec![("X-Poll-Interval", "60".to_string())],
        },
        Rule {
            path_needle: "commits/beef34/check-runs",
            requires_etag: Some("\"c2\"".to_string()),
            status: 200,
            body: check_runs_body(700, "failure"),
            headers: vec![("ETag", "\"c2\"".to_string())],
        },
    ])
    .await;
    let (queue, _store, lane) = fixture_lane();
    let target = target_of(&server);

    // Round 1: runs poll tracks beef34; the check-runs poll is the
    // first-enable baseline (the pre-existing success registers, emits
    // nothing).
    let mut first = lane.poll_repo(&target, 1_000).await.expect("round 1");
    assert_eq!(first.len(), 2, "runs poll + one tracked SHA's check-runs");
    let baseline = first.remove(1).expect("check-runs baseline ok");
    assert_eq!(
        baseline.accepted, 0,
        "the first-enable baseline enqueues nothing"
    );

    // Round 2: the same check run now reports `failure` — a NEW delivery.
    let mut second = lane.poll_repo(&target, 1_100).await.expect("round 2");
    let transition = second.remove(1).expect("check-runs transition ok");
    assert_eq!(
        (transition.accepted, transition.duplicates),
        (1, 0),
        "the conclusion transition is a new delivery (the transition IS the signal)"
    );
    let drained = queue.take();
    // Two items: round 1's runs poll accepted the completed workflow run
    // (900, PR 504) — it stays pending until the kick-off consumer
    // drains — plus round 2's check-run transition.
    assert_eq!(drained.len(), 2);
    let check_event = drained
        .iter()
        .find(|event| event.event_type == "check-run-completed")
        .expect("the check-run transition event is in the queue");
    assert_eq!(check_event.number, 505);
    assert_eq!(
        check_event.delivery_key, "700:failure|arbsec/orchestraitor|check-run-completed|beef34",
        "the check-run key carries check-run id + conclusion + repo + type + head SHA"
    );

    // Round 3 (same conclusion, re-poll): deduplicated, never a second
    // kick-off.
    let mut third = lane.poll_repo(&target, 1_200).await.expect("round 3");
    let repoll = third.remove(1).expect("check-runs repoll ok");
    assert_eq!(
        (repoll.accepted, repoll.duplicates),
        (0, 1),
        "a repeated poll of the unchanged check run is a redelivery, not an event"
    );
}

/// AC: rate-limit feedback (§9.43) drives the lane's cadence — a 403
/// with `X-RateLimit-Remaining: 0` waits exactly until the reset (never
/// wider), and the durable bookkeeping carries the adapted wait.
#[tokio::test(flavor = "current_thread")]
async fn rate_limit_feedback_adapts_the_lane_cadence() {
    let now = 1_000_000_u64;
    let reset = now + 120;
    let server = FixtureServer::start(vec![Rule {
        path_needle: "actions/runs",
        requires_etag: None,
        status: 403,
        body: String::new(),
        headers: vec![
            ("X-RateLimit-Remaining", "0".to_string()),
            ("X-RateLimit-Reset", reset.to_string()),
        ],
    }])
    .await;
    let (_queue, store, lane) = fixture_lane();
    let target = target_of(&server);

    let outcome = lane
        .poll_repo(&target, now)
        .await
        .expect("poll round")
        .remove(0)
        .expect_err("rate-limited is an error outcome");
    match outcome {
        PollError::RateLimited { retry_after_secs } => {
            assert_eq!(
                retry_after_secs, 120,
                "the wait is exactly until the reset, never wider"
            );
        }
        PollError::Failed => panic!("a rate limit must surface as RateLimited, not Failed"),
    }
    let stored = store
        .lock()
        .expect("store lock")
        .poll_after_secs(&target.runs_key)
        .expect("store read");
    assert_eq!(
        stored, 120,
        "the durable bookkeeping carries the rate-limit wait"
    );
}

/// AC: an explicit `Retry-After` hint wins over the reset epoch (the
/// events lane's precedence, pinned for the CI lane too).
#[tokio::test(flavor = "current_thread")]
async fn retry_after_hint_drives_the_lane_cadence() {
    let now = 1_000_000_u64;
    let server = FixtureServer::start(vec![Rule {
        path_needle: "actions/runs",
        requires_etag: None,
        status: 429,
        body: String::new(),
        headers: vec![("Retry-After", "45".to_string())],
    }])
    .await;
    let (_queue, _store, lane) = fixture_lane();
    let target = target_of(&server);

    let outcome = lane
        .poll_repo(&target, now)
        .await
        .expect("poll round")
        .remove(0)
        .expect_err("rate-limited is an error outcome");
    assert_eq!(
        outcome,
        PollError::RateLimited {
            retry_after_secs: 45
        },
        "the explicit Retry-After wins"
    );
}

/// AC: a 304 on the runs endpoint costs no quota equivalent in the
/// lane's bookkeeping — no events, zero accepted, the stored `ETag` is
/// replayed on the next request.
#[tokio::test(flavor = "current_thread")]
async fn conditional_request_sends_stored_etag_on_next_lane_poll() {
    let etag = "\"runs-1\"";
    let server = FixtureServer::start(vec![
        Rule {
            path_needle: "actions/runs",
            requires_etag: None,
            status: 200,
            body: runs_body(),
            headers: vec![("ETag", etag.to_string())],
        },
        Rule {
            path_needle: "actions/runs",
            requires_etag: Some(etag.to_string()),
            status: 304,
            body: String::new(),
            headers: vec![("X-Poll-Interval", "60".to_string())],
        },
    ])
    .await;
    let (_queue, _store, lane) = fixture_lane();
    let target = target_of(&server);

    let first = lane
        .poll_repo(&target, 1_000)
        .await
        .expect("first round")
        .remove(0)
        .expect("first poll ok");
    assert_eq!(first.accepted, 1);

    let second = lane.poll_repo(&target, 1_100).await.expect("second round");
    // The 304 runs poll emits nothing; the tracked SHA's check-runs poll
    // still fires (tracked SHAs survive a 304 round; unscripted here so
    // it answers 500 -> Failed).
    let runs_outcome = &second[0];
    assert!(
        matches!(runs_outcome, Ok(outcome) if outcome.accepted == 0 && outcome.duplicates == 0),
        "the 304 produces no events"
    );
    assert_eq!(runs_outcome.clone().expect("ok").poll_after_secs, 60);
    assert!(
        second.len() == 2 && matches!(second[1], Err(PollError::Failed)),
        "the tracked SHA's check-runs poll still fires after a 304 runs poll"
    );
    let requests = server.requests();
    // Round 1: runs-200 then the unscripted check-runs probe; round 2:
    // runs-304 then the second check-runs probe. The runs request is
    // requests[2].
    assert_eq!(
        requests[2].if_none_match.as_deref(),
        Some(etag),
        "the second runs request sends the stored ETag"
    );
}

/// AC (§21.4 negative): an unshaped item (missing id, no PR number,
/// fork-PR empty `pull_requests`) is SKIPPED, never folded into a shared
/// empty delivery key.
#[tokio::test(flavor = "current_thread")]
async fn unshaped_run_items_are_skipped_never_shared() {
    let body = json!({
        "workflow_runs": [
            { "id": 902, "status": "completed", "conclusion": "success",
              "event": "pull_request", "head_sha": "aa11", "run_attempt": 1,
              "pull_requests": [] },
            { "status": "completed", "conclusion": "success",
              "event": "pull_request", "head_sha": "bb22", "run_attempt": 1,
              "pull_requests": [ { "number": 506 } ] },
            { "id": 903, "status": "completed", "conclusion": "success",
              "event": "push", "head_sha": "cc33", "run_attempt": 1,
              "pull_requests": [ { "number": 507 } ] }
        ]
    })
    .to_string();
    let server = FixtureServer::start(vec![Rule {
        path_needle: "actions/runs",
        requires_etag: None,
        status: 200,
        body,
        headers: vec![("X-Poll-Interval", "60".to_string())],
    }])
    .await;
    let (queue, _store, lane) = fixture_lane();
    let target = target_of(&server);

    let outcome = lane
        .poll_repo(&target, 1_000)
        .await
        .expect("poll round")
        .remove(0)
        .expect("runs poll ok");
    assert_eq!(
        (outcome.accepted, outcome.duplicates, outcome.dropped),
        (0, 0, 0),
        "fork-PR (empty pull_requests), id-less, and non-pull_request runs are all skipped"
    );
    assert!(queue.peek().is_empty(), "nothing reached the queue");
}
