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
    path: String,
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
        path: path.clone(),
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
            path_needle: "commits/beef34/check-runs",
            requires_etag: None,
            status: 200,
            body: check_runs_body(701, "success"),
            headers: vec![("ETag", "\"q1\"".to_string())],
        },
        Rule {
            path_needle: "actions/runs",
            requires_etag: Some("\"r1\"".to_string()),
            status: 200,
            body: runs_body(),
            headers: vec![("ETag", "\"r1\"".to_string())],
        },
        Rule {
            path_needle: "commits/beef34/check-runs",
            requires_etag: None,
            status: 200,
            body: check_runs_body(701, "success"),
            headers: vec![("ETag", "\"q1\"".to_string())],
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

    let mut second = lane.poll_repo(&target, 1_100).await.expect("second round");
    let _requests = server.requests();
    let runs2 = second.remove(0).expect("runs poll ok");
    assert_eq!(
        (runs2.accepted, runs2.duplicates),
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

/// Regression (review finding): the tracked set is the
/// UNION of the previous SHAs and the newly in-flight SHAs — a SHA whose
/// last run completed keeps its check-runs polls (its final Actions job
/// and third-party checks may complete after the run).
#[tokio::test(flavor = "current_thread")]
async fn a_sha_whose_run_completes_stays_tracked() {
    // Round 1: beef34 is in-flight, so it becomes tracked and its
    // check-runs poll runs (first poll: a 200 baseline).
    let server = FixtureServer::start(vec![
        Rule {
            path_needle: "actions/runs",
            requires_etag: None,
            status: 200,
            body: runs_body(),
            headers: vec![("X-Poll-Interval", "60".to_string())],
        },
        Rule {
            path_needle: "commits/beef34/check-runs",
            requires_etag: None,
            status: 200,
            body: check_runs_body(701, "success"),
            headers: vec![("ETag", "\"q1\"".to_string())],
        },
        // Round 2: NO in-flight runs (all completed) — beef34 must STILL
        // be tracked (union) and get a check-runs poll.
        Rule {
            path_needle: "actions/runs",
            requires_etag: None,
            status: 200,
            body: json!({"workflow_runs": [{
                "id": 901, "status": "completed", "conclusion": "success",
                "event": "pull_request", "head_sha": "beef34",
                "run_attempt": 1, "pull_requests": [{"number": 505}]
            }]})
            .to_string(),
            headers: vec![("X-Poll-Interval", "60".to_string())],
        },
        Rule {
            path_needle: "commits/beef34/check-runs",
            requires_etag: Some("\"q1\"".to_string()),
            status: 304,
            body: String::new(),
            headers: vec![("X-Poll-Interval", "60".to_string())],
        },
    ])
    .await;
    let (queue, _store, lane) = fixture_lane();
    let target = target_of(&server);

    let mut first = lane.poll_repo(&target, 1_000).await.expect("round 1");
    assert_eq!(first.len(), 2, "runs + tracked beef34 check-runs");
    let _ = first.remove(1).expect("baseline ok");

    let second = lane.poll_repo(&target, 1_100).await.expect("round 2");
    assert_eq!(
        second.len(),
        2,
        "beef34 stays tracked after its run completed (union tracking)"
    );
    assert!(
        matches!(second[1], Ok(ref o) if o.accepted == 0 && o.duplicates == 0),
        "the 304 check-runs poll produces no events"
    );
    let _ = queue.take();
}

/// Regression (review finding): duplicate
/// in-flight SHAs are deduplicated BEFORE the 16-SHA cap.
#[tokio::test(flavor = "current_thread")]
async fn duplicate_in_flight_shas_do_not_spend_the_cap() {
    // 20 rows, all in-flight, but only 17 distinct SHAs. Dedup-before-cap
    // keeps 16 of the 17 DISTINCT SHAs tracked (never 16 rows-worth with
    // duplicates).
    let mut runs = Vec::new();
    for i in 0..20u64 {
        let sha = format!("sha{:02}", i % 17); // 17 distinct, 20 rows
        runs.push(json!({
            "id": 1000 + i, "status": "in_progress", "event": "pull_request",
            "head_sha": sha, "run_attempt": 1,
            "pull_requests": [{"number": 600 + i}]
        }));
    }
    let body = json!({ "workflow_runs": runs }).to_string();
    let server = FixtureServer::start(vec![Rule {
        path_needle: "actions/runs",
        requires_etag: None,
        status: 200,
        body,
        headers: vec![("X-Poll-Interval", "60".to_string())],
    }])
    .await;
    let (_queue, _store, lane) = fixture_lane();
    let target = target_of(&server);

    // The first poll fires check-runs for the tracked SHAs; the fixture
    // scripts none of them (each answers 500 -> Failed). Count the
    // DISTINCT check-run request paths.
    let _ = lane.poll_repo(&target, 1_000).await.expect("round");
    let requests = server.requests();
    let distinct_check_polls = requests
        .iter()
        .filter(|r| r.path.contains("/check-runs"))
        .map(|r| r.path.clone())
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(
        distinct_check_polls.len(),
        16,
        "exactly the capped count of DISTINCT SHAs polled (dedup before cap)"
    );
}

/// Regression (review finding): the FIRST poll of a
/// process bypasses the persisted `runs` `ETag` — a restart must not hide
/// active completions behind a 304.
#[tokio::test(flavor = "current_thread")]
async fn first_poll_of_a_process_bypasses_the_persisted_runs_etag() {
    let etag = "\"persisted\"";
    // Two rounds: round 1 (bypass — no If-None-Match even though the
    // store would carry an ETag from a previous process), round 2 rides
    // the ETag normally.
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
    // Simulate a restart: the STORE carries the ETag, the lane is fresh.
    let (_queue, store, lane) = fixture_lane();
    store
        .lock()
        .expect("store lock")
        .record_conditional("arbsec/orchestraitor/actions-runs", Some(etag), 60, true)
        .expect("seed persisted etag");
    let target = target_of(&server);

    let first = lane
        .poll_repo(&target, 1_000)
        .await
        .expect("first round")
        .remove(0)
        .expect("runs poll ok");
    assert_eq!(
        first.accepted, 1,
        "the first poll bypassed the persisted ETag (200, not 304) and accepted the completion"
    );
    let requests = server.requests();
    assert!(
        requests[0].if_none_match.is_none(),
        "the first runs request carries no If-None-Match (bypass)"
    );
}

/// Regression (review finding): the runs endpoint is marked initialized
/// ONLY after a successful 200 — a failed first poll (here: 403 rate
/// limit) leaves the bypass gate open, so the next poll re-discovers.
#[tokio::test(flavor = "current_thread")]
async fn failed_first_poll_keeps_the_etag_bypass_open() {
    let now = 1_000_000_u64;
    let server = FixtureServer::start(vec![
        Rule {
            path_needle: "actions/runs",
            requires_etag: None,
            status: 429,
            body: String::new(),
            headers: vec![("Retry-After", "1".to_string())],
        },
        Rule {
            path_needle: "actions/runs",
            requires_etag: None,
            status: 200,
            body: runs_body(),
            headers: vec![("ETag", "\"late\"".to_string())],
        },
    ])
    .await;
    // A PERSISTED ETag from a previous process exists in the store.
    let (_queue, store, lane) = fixture_lane();
    store
        .lock()
        .expect("store lock")
        .record_conditional(
            "arbsec/orchestraitor/actions-runs",
            Some("\"persisted\""),
            60,
            true,
        )
        .expect("seed persisted etag");
    let target = target_of(&server);

    // First poll: rate-limited (429) — the gate must stay OPEN.
    let first = lane
        .poll_repo(&target, now)
        .await
        .expect("first round")
        .remove(0);
    assert!(
        matches!(first, Err(PollError::RateLimited { .. })),
        "the first poll is rate-limited"
    );
    // Second poll: still bypassing (the request carries NO If-None-Match —
    // the fixture rule requires None), gets the 200 and recovers.
    let second = lane
        .poll_repo(&target, now + 2)
        .await
        .expect("second round")
        .remove(0)
        .expect("runs poll ok");
    assert_eq!(
        second.accepted, 1,
        "the post-failure poll bypassed the persisted ETag and discovered the completion"
    );
    let requests = server.requests();
    assert!(
        requests[1].if_none_match.is_none(),
        "the post-failure request still bypasses the persisted ETag"
    );
}

/// Regression (review finding): tracked SHAs evict by AGE (the tail of
/// the newest-first list), not by `HashSet` iteration order.
#[tokio::test(flavor = "current_thread")]
async fn cap_eviction_is_deterministic_oldest_first() {
    // Round 1: SHA "old" in-flight (tracked). Round 2: 16 NEW in-flight
    // SHAs arrive; "old" completes. Round 3: 16 more NEW SHAs. "old" was
    // the oldest tracked SHA — it must be the one evicted, deterministically.
    let mk = |shas: &[&str]| {
        let runs: Vec<serde_json::Value> = shas
            .iter()
            .enumerate()
            .map(|(i, sha)| {
                json!({
                    "id": i as u64, "status": "in_progress", "event": "pull_request",
                    "head_sha": sha, "run_attempt": 1,
                    "pull_requests": [{"number": 700 + i as u64}]
                })
            })
            .collect();
        json!({ "workflow_runs": runs }).to_string()
    };
    let round1 = mk(&["old"]);
    let round2_shas: Vec<String> = (0..16).map(|i| format!("n{i:02}")).collect();
    let round2 = mk(&round2_shas.iter().map(String::as_str).collect::<Vec<_>>());
    let round3_shas: Vec<String> = (16..32).map(|i| format!("n{i:02}")).collect();
    let round3 = mk(&round3_shas.iter().map(String::as_str).collect::<Vec<_>>());
    let server = FixtureServer::start(vec![
        Rule {
            path_needle: "actions/runs",
            requires_etag: None,
            status: 200,
            body: round1,
            headers: vec![("X-Poll-Interval", "60".to_string())],
        },
        Rule {
            path_needle: "actions/runs",
            requires_etag: None,
            status: 200,
            body: round2,
            headers: vec![("X-Poll-Interval", "60".to_string())],
        },
        Rule {
            path_needle: "actions/runs",
            requires_etag: None,
            status: 200,
            body: round3,
            headers: vec![("X-Poll-Interval", "60".to_string())],
        },
    ])
    .await;
    let (_queue, _store, lane) = fixture_lane();
    let target = target_of(&server);
    let _ = lane.poll_repo(&target, 1_000).await.expect("round 1");
    let _ = lane.poll_repo(&target, 1_100).await.expect("round 2");
    // Round 3: the union is 16 (n00..n15) + 1 (old) + 16 (n16..n31) = 33;
    // the cap keeps the newest 16 (n16..n31). Deterministic eviction of
    // the oldest — asserted by the poll count being exactly 16.
    let _ = lane.poll_repo(&target, 1_200).await.expect("round 3");
    let check_paths: Vec<String> = server
        .requests()
        .iter()
        .filter(|r| r.path.contains("/check-runs"))
        .map(|r| r.path.clone())
        .collect();
    // Round 1: 1 (old). Round 2: the union is old + n00..n15 = 17 > cap 16,
    // so `old` is evicted (oldest first) -> 16 (n00..n15). Round 3: the
    // union is n00..n15 + n16..n31 = 32 > 16 -> the oldest half evicted
    // -> 16 (n16..n31).
    assert_eq!(
        check_paths.len(),
        1 + 16 + 16,
        "cap evicts the OLDEST SHAs deterministically, never by hash order"
    );
    // Round 3 polled EXACTLY n16..n31 — not `old`, not n00..n15.
    let round3 = &check_paths[17..];
    for i in 16..32 {
        let expected = format!("/repos/arbsec/orchestraitor/commits/n{i:02}/check-runs");
        assert!(
            round3.iter().any(|p| p.contains(&expected)),
            "round 3 polled {expected}"
        );
    }
    assert!(
        round3
            .iter()
            .all(|p| { (16..32).any(|i| p.contains(&format!("/commits/n{i:02}/check-runs"))) }),
        "round 3 never polled the evicted SHAs: {round3:?}"
    );
}

/// Regression (review finding): a SHA that is BOTH in-flight this round
/// AND answered 304 on check-runs is NOT pruned — an in-flight run can
/// still produce completions after its runs finish.
#[tokio::test(flavor = "current_thread")]
async fn a_304_check_runs_sha_still_in_flight_is_not_pruned() {
    let server = FixtureServer::start(vec![
        // Round 1: beef34 in-flight -> tracked; its check-runs 200 baseline.
        Rule {
            path_needle: "actions/runs",
            requires_etag: None,
            status: 200,
            body: runs_body(),
            headers: vec![("X-Poll-Interval", "60".to_string())],
        },
        Rule {
            path_needle: "commits/beef34/check-runs",
            requires_etag: None,
            status: 200,
            body: check_runs_body(702, "success"),
            headers: vec![("ETag", "\"z1\"".to_string())],
        },
        // Round 2: beef34 STILL in-flight AND its check-runs 304.
        Rule {
            path_needle: "actions/runs",
            requires_etag: None,
            status: 200,
            body: runs_body(),
            headers: vec![("X-Poll-Interval", "60".to_string())],
        },
        Rule {
            path_needle: "commits/beef34/check-runs",
            requires_etag: Some("\"z1\"".to_string()),
            status: 304,
            body: String::new(),
            headers: vec![("X-Poll-Interval", "60".to_string())],
        },
        // Round 3: beef34 still in-flight — its check-runs MUST be polled
        // again (the 304 must not have pruned it).
        Rule {
            path_needle: "actions/runs",
            requires_etag: None,
            status: 200,
            body: runs_body(),
            headers: vec![("X-Poll-Interval", "60".to_string())],
        },
        Rule {
            path_needle: "commits/beef34/check-runs",
            requires_etag: Some("\"z1\"".to_string()),
            status: 200,
            body: check_runs_body(703, "failure"),
            headers: vec![("ETag", "\"z2\"".to_string())],
        },
    ])
    .await;
    let (queue, _store, lane) = fixture_lane();
    let target = target_of(&server);
    let _ = lane.poll_repo(&target, 1_000).await.expect("round 1");
    let _ = lane.poll_repo(&target, 1_100).await.expect("round 2");
    let mut third = lane.poll_repo(&target, 1_200).await.expect("round 3");
    assert_eq!(
        third.len(),
        2,
        "the 304 + still-in-flight SHA was NOT pruned: its check-runs poll fires again"
    );
    let checks = third.remove(1).expect("check-runs poll ok");
    assert_eq!(
        (checks.accepted, checks.duplicates),
        (1, 0),
        "the newly-completed check run (703:failure) is delivered"
    );
    // Queue: round 1's baseline emits nothing and round 2's 304 emits
    // nothing, but round 1's runs poll DID enqueue the `900:success`
    // completion (still pending — never drained) and round 3 delivers the
    // `703:failure` check completion: two pending events.
    assert_eq!(queue.take().len(), 2);
}

/// Regression (review finding): a runs 304 restores the previous tracked
/// set and does NOT treat the unavailable snapshot as an empty in-flight
/// list — a quiet-SHA prune must not fire on a snapshot-less round.
#[tokio::test(flavor = "current_thread")]
async fn a_runs_304_restores_the_tracked_set_and_skips_quiet_pruning() {
    let etag = "\"runs-snap\"";
    let server = FixtureServer::start(vec![
        // Round 1: 200 with beef34 in-flight -> tracked; checks baseline 200.
        Rule {
            path_needle: "actions/runs",
            requires_etag: None,
            status: 200,
            body: runs_body(),
            headers: vec![("ETag", etag.to_string())],
        },
        Rule {
            path_needle: "commits/beef34/check-runs",
            requires_etag: None,
            status: 200,
            body: check_runs_body(704, "success"),
            headers: vec![("ETag", "\"w1\"".to_string())],
        },
        // Round 2: runs 304 (no snapshot); checks 304.
        Rule {
            path_needle: "actions/runs",
            requires_etag: Some(etag.to_string()),
            status: 304,
            body: String::new(),
            headers: vec![("X-Poll-Interval", "60".to_string())],
        },
        Rule {
            path_needle: "commits/beef34/check-runs",
            requires_etag: Some("\"w1\"".to_string()),
            status: 304,
            body: String::new(),
            headers: vec![("X-Poll-Interval", "60".to_string())],
        },
        // Round 3: the tracked set SURVIVED the snapshot-less round —
        // beef34's check-runs poll fires again (and delivers).
        Rule {
            path_needle: "actions/runs",
            requires_etag: Some(etag.to_string()),
            status: 304,
            body: String::new(),
            headers: vec![("X-Poll-Interval", "60".to_string())],
        },
        Rule {
            path_needle: "commits/beef34/check-runs",
            requires_etag: Some("\"w1\"".to_string()),
            status: 200,
            body: check_runs_body(705, "failure"),
            headers: vec![("ETag", "\"w2\"".to_string())],
        },
    ])
    .await;
    let (queue, _store, lane) = fixture_lane();
    let target = target_of(&server);
    let _ = lane.poll_repo(&target, 1_000).await.expect("round 1");
    let _ = lane.poll_repo(&target, 1_100).await.expect("round 2");
    let mut third = lane.poll_repo(&target, 1_200).await.expect("round 3");
    assert_eq!(
        third.len(),
        2,
        "the tracked set survived the snapshot-less round: beef34 is polled again"
    );
    let checks = third.remove(1).expect("check-runs poll ok");
    assert_eq!(
        (checks.accepted, checks.duplicates),
        (1, 0),
        "the completion that arrived during the snapshot-less rounds is delivered"
    );
    assert_eq!(queue.take().len(), 2); // round-1 run 900 + this check 705
}

/// Regression (review finding): a runs 304 REUSES the last valid
/// snapshot — a SHA that went quiet under the last snapshot (not
/// re-discovered in-flight) is pruned on the 304 round's matching
/// check-runs 304.
#[tokio::test(flavor = "current_thread")]
async fn a_runs_304_reuses_the_last_snapshot_for_quiet_pruning() {
    let etag = "\"snap-x\"";
    // Round 1: beef34 in-flight -> tracked; checks baseline.
    // Round 2: beef34 NOT in the runs response (its run finished), check-runs 200
    //          (not quiet yet — the run just completed).
    // Round 3: runs 304 (snapshot reused: beef34 absent -> NOT in-flight);
    //          check-runs 304 -> quiet prune fires.
    // Round 4: beef34 no longer polled (bookkeeping pruned).
    let empty_runs = json!({ "workflow_runs": [] }).to_string();
    let server = FixtureServer::start(vec![
        Rule {
            path_needle: "actions/runs",
            requires_etag: None,
            status: 200,
            body: runs_body(),
            headers: vec![("ETag", etag.to_string())],
        },
        Rule {
            path_needle: "commits/beef34/check-runs",
            requires_etag: None,
            status: 200,
            body: check_runs_body(706, "success"),
            headers: vec![("ETag", "\"y1\"".to_string())],
        },
        Rule {
            path_needle: "actions/runs",
            requires_etag: Some(etag.to_string()),
            status: 200,
            body: empty_runs,
            headers: vec![("ETag", etag.to_string())],
        },
        Rule {
            path_needle: "commits/beef34/check-runs",
            requires_etag: Some("\"y1\"".to_string()),
            status: 200,
            body: check_runs_body(707, "success"),
            headers: vec![("ETag", "\"y2\"".to_string())],
        },
        Rule {
            path_needle: "actions/runs",
            requires_etag: Some(etag.to_string()),
            status: 304,
            body: String::new(),
            headers: vec![("X-Poll-Interval", "60".to_string())],
        },
        Rule {
            path_needle: "commits/beef34/check-runs",
            requires_etag: Some("\"y2\"".to_string()),
            status: 304,
            body: String::new(),
            headers: vec![("X-Poll-Interval", "60".to_string())],
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
    let _ = lane.poll_repo(&target, 1_000).await.expect("round 1");
    let _ = lane.poll_repo(&target, 1_100).await.expect("round 2");
    let _ = lane
        .poll_repo(&target, 1_200)
        .await
        .expect("round 3 (304 + 304 -> quiet)");
    let fourth = lane.poll_repo(&target, 1_300).await.expect("round 4");
    assert_eq!(
        fourth.len(),
        1,
        "the quiet SHA was pruned on the 304 round: only the runs poll fires in round 4"
    );
}
