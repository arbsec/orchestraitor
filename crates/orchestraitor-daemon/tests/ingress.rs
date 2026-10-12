//! Fixture-driven integration tests for the §9.47 event ingress (issue
//! #504, spec §21.3): the conditional-request poller runs against a
//! scripted in-process HTTP server — no live network in CI. Negative
//! and adversarial cases (§21.4) pin the dedup, flood, and rate-limit
//! contracts.

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
use orchestraitor_daemon::ingress::poller::{EventPoller, PollTarget, poll_targets};
use orchestraitor_daemon::ingress::queue::{IngressQueue, PushOutcome};
use orchestraitor_daemon::ingress::store::{IngressStore, REDELIVERY_WINDOW_SECS};
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
    authorization: Option<String>,
}

/// One scripted response rule.
struct Rule {
    /// Requests whose path contains this needle match the rule.
    path_needle: &'static str,
    /// `If-None-Match` value the rule requires (`None` = matches any).
    requires_etag: Option<String>,
    /// HTTP status to serve.
    status: u16,
    /// Response body (empty for 304s).
    body: String,
    /// Response headers.
    headers: Vec<(&'static str, String)>,
}

/// A scripted HTTP fixture server on `127.0.0.1` (ephemeral port). Each
/// request consumes the FIRST unconsumed matching rule; when no rule
/// matches, the server answers 500 (a test must script every request it
/// causes — an unmatched request is a test bug, never a silent pass).
struct FixtureServer {
    base: String,
    captured: Arc<std::sync::Mutex<Vec<Captured>>>,
    shutdown: Arc<std::sync::atomic::AtomicBool>,
    join: Option<tokio::task::JoinHandle<()>>,
}

impl FixtureServer {
    /// Starts the server with the given rules.
    ///
    /// # Panics
    ///
    /// Panics when the listener cannot bind (test-environment failure).
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

    /// Every captured request, in arrival order.
    fn requests(&self) -> Vec<Captured> {
        self.captured.lock().expect("captured lock").clone()
    }

    /// Builds the poll target pointing at this server (repo-scoped, the
    /// production shape).
    fn target(&self, key: &'static str, collection: &str) -> PollTarget {
        let repo = "arbsec/orchestraitor";
        PollTarget {
            url: format!("{}/repos/{repo}/{collection}/events", self.base),
            key: format!("{repo}/{key}"),
            repo: repo.to_string(),
        }
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
    let authorization = req
        .headers()
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .map(std::string::ToString::to_string);
    let path = req.uri().path().to_string();
    captured.lock().expect("captured lock").push(Captured {
        if_none_match: if_none_match.clone(),
        authorization: authorization.clone(),
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

/// A two-item issues-event array fixture (one issue comment, one open).
fn issues_events_body() -> String {
    json!([
        {
            "id": "111",
            "event": "commented",
            "issue": { "number": 504 }
        },
        {
            "id": "222",
            "event": "opened",
            "issue": { "number": 505 }
        }
    ])
    .to_string()
}

/// Builds a fresh queue + store + poller triple against in-memory state.
fn fixture_ingress() -> (
    Arc<IngressQueue>,
    Arc<std::sync::Mutex<IngressStore>>,
    EventPoller,
) {
    let queue = Arc::new(IngressQueue::new(8).expect("valid capacity"));
    let store = Arc::new(std::sync::Mutex::new(
        IngressStore::open_in_memory().expect("in-memory store"),
    ));
    let poller = EventPoller::new(Arc::new(StaticAuth), Arc::clone(&store), Arc::clone(&queue))
        .expect("poller builds");
    (queue, store, poller)
}

/// AC 1 (§21.3): the FIRST request carries no `If-None-Match`; a `200`
/// with a new `ETag` is stored, and the NEXT request sends it. The body's
/// events pass dedup into the queue.
#[tokio::test(flavor = "current_thread")]
async fn conditional_request_sends_stored_etag_on_next_poll() {
    let etag = "\"abc123\"";
    let server = FixtureServer::start(vec![
        Rule {
            path_needle: "arbsec/orchestraitor/issues/events",
            requires_etag: None,
            status: 200,
            body: issues_events_body(),
            headers: vec![
                ("ETag", etag.to_string()),
                ("X-Poll-Interval", "60".to_string()),
            ],
        },
        Rule {
            path_needle: "arbsec/orchestraitor/issues/events",
            requires_etag: Some(etag.to_string()),
            status: 304,
            body: String::new(),
            headers: vec![("X-Poll-Interval", "60".to_string())],
        },
    ])
    .await;
    let (_queue, _store, poller) = fixture_ingress();
    let target = server.target("issues-events", "issues");

    let first = poller
        .poll_all(std::slice::from_ref(&target), 1_000)
        .await
        .expect("poll round")
        .remove(0)
        .expect("first poll ok");
    assert_eq!(first.accepted, 2, "both fixture events accepted");

    let second = poller
        .poll_all(std::slice::from_ref(&target), 1_100)
        .await
        .expect("poll round")
        .remove(0)
        .expect("second poll ok");
    assert_eq!(
        (second.accepted, second.duplicates),
        (0, 0),
        "a 304 produces no events"
    );
    assert_eq!(
        second.poll_after_secs, 60,
        "the honored X-Poll-Interval drives the next wait"
    );

    let requests = server.requests();
    assert_eq!(requests.len(), 2, "exactly the two scripted polls");
    assert!(
        requests[0].if_none_match.is_none(),
        "the first request carries no conditional header"
    );
    assert_eq!(
        requests[1].if_none_match.as_deref(),
        Some(etag),
        "the second request sends the stored ETag (If-None-Match)"
    );
    // The auth rides the board provider's seam (the poller reads
    // board-visible events with the board provider's credentials).
    assert_eq!(
        requests[0].authorization.as_deref(),
        Some("Bearer fixture-token-value"),
        "the poller authenticates through the injected BoardAuth"
    );
}

/// AC 1 (§21.3): the server-advised `X-Poll-Interval` is honored — the
/// per-endpoint stored wait equals the advised value, and the next-wait
/// read reflects it.
#[tokio::test(flavor = "current_thread")]
async fn poll_interval_header_is_honored() {
    let server = FixtureServer::start(vec![Rule {
        path_needle: "arbsec/orchestraitor/issues/events",
        requires_etag: None,
        status: 200,
        body: issues_events_body(),
        headers: vec![("X-Poll-Interval", "90".to_string())],
    }])
    .await;
    let (_queue, store, poller) = fixture_ingress();
    let target = server.target("issues-events", "issues");
    let outcome = poller
        .poll_all(std::slice::from_ref(&target), 1_000)
        .await
        .expect("poll round")
        .remove(0)
        .expect("poll ok");
    assert_eq!(outcome.poll_after_secs, 90);
    let store = store.lock().expect("store lock");
    assert_eq!(
        store
            .poll_after_secs("arbsec/orchestraitor/issues-events")
            .expect("read"),
        90,
        "the advised interval persists in the durable bookkeeping"
    );
    drop(store);
    assert_eq!(
        poller
            .next_poll_wait(std::slice::from_ref(&target))
            .expect("wait read"),
        Duration::from_secs(90),
    );
}

/// AC 2 (§21.4 negative): a redelivered event — the same delivery key —
/// never produces a second accepted event. The second poll returns
/// duplicates = 1, accepted = 0 for the repeat.
#[tokio::test(flavor = "current_thread")]
async fn a_redelivered_event_never_produces_a_second_kick_off() {
    let body =
        json!([{ "id": "111", "event": "commented", "issue": { "number": 504 } }]).to_string();
    let server = FixtureServer::start(vec![
        Rule {
            path_needle: "arbsec/orchestraitor/issues/events",
            requires_etag: None,
            status: 200,
            body: body.clone(),
            headers: vec![("X-Poll-Interval", "60".to_string())],
        },
        Rule {
            path_needle: "arbsec/orchestraitor/issues/events",
            requires_etag: None,
            status: 200,
            body,
            headers: vec![("X-Poll-Interval", "60".to_string())],
        },
    ])
    .await;
    let (queue, _store, poller) = fixture_ingress();
    let target = server.target("issues-events", "issues");

    let first = poller
        .poll_all(std::slice::from_ref(&target), 1_000)
        .await
        .expect("poll round")
        .remove(0)
        .expect("first poll ok");
    assert_eq!(first.accepted, 1, "the fresh event is accepted once");
    assert_eq!(queue.take().expect("drain").len(), 1);

    let second = poller
        .poll_all(std::slice::from_ref(&target), 2_000)
        .await
        .expect("poll round")
        .remove(0)
        .expect("second poll ok");
    assert_eq!(
        (second.accepted, second.duplicates),
        (0, 1),
        "the redelivery deduplicates inside the 30-day window"
    );
    assert!(
        queue.take().expect("drain").is_empty(),
        "the redelivery never reaches the kick-off queue (forbidden effect: \
         a second kick-off for the same delivery)"
    );
}

/// AC 2 (§21.4): the redelivery window covers the platform's 30-day
/// horizon — a key seen OUTSIDE the window (simulated by pruning clock
/// movement) is accepted again; the store prunes on insert.
#[tokio::test(flavor = "current_thread")]
async fn the_redelivery_window_covers_thirty_days_and_prunes() {
    let mut store = IngressStore::open_in_memory().expect("store");
    let key = "111|arbsec/orchestraitor|issue-commented|";
    assert!(
        store.record_delivery(key, 1_000).expect("record"),
        "first sight is accepted"
    );
    assert!(
        !store
            .record_delivery(key, 1_000 + REDELIVERY_WINDOW_SECS - 1)
            .expect("record"),
        "inside the window: deduplicated"
    );
    assert!(
        store
            .record_delivery(key, 1_000 + REDELIVERY_WINDOW_SECS + 1)
            .expect("record"),
        "outside the window: a genuinely new platform delivery"
    );
}

/// AC 3 (§21.4): sustained flooding drops surplus and records
/// `degraded`; the marker recovers after the flood-free quiet window.
#[tokio::test]
async fn flood_drops_surplus_records_degraded_and_recovers() {
    let queue = IngressQueue::new(2).expect("valid capacity");
    let event = |n: u64| orchestraitor_daemon::IngressEvent {
        delivery_key: format!("key-{n}"),
        repo: "arbsec/orchestraitor".to_string(),
        event_type: "issue-commented".to_string(),
        head_sha: None,
        number: n,
        accepted_at_secs: n,
    };
    // Fill to capacity, then flood.
    assert_eq!(
        queue.push(event(1), 1_000).expect("push"),
        PushOutcome::Accepted
    );
    assert_eq!(
        queue.push(event(2), 1_000).expect("push"),
        PushOutcome::Accepted
    );
    assert_eq!(
        queue.push(event(3), 1_100).expect("push"),
        PushOutcome::Dropped,
        "the surplus is dropped at capacity, never queued"
    );
    assert!(
        queue.degraded(1_100).expect("degraded"),
        "the degraded marker is recorded on the overflow"
    );
    assert!(
        queue.degraded(1_100 + 299).expect("degraded"),
        "still degraded inside the flood-free window"
    );
    // The queue drains (the kick-off consumed the accepted events).
    let drained = queue.take().expect("drain");
    assert_eq!(drained.len(), 2, "the accepted events survive the flood");
    assert!(
        queue.degraded(1_100 + 299).expect("degraded"),
        "the marker survives a drain inside the window (only a flood-free \
         window clears it)"
    );
    // The §7.3 forbidden effect — unbounded growth — is structurally
    // impossible: capacity is fixed; assert the bound holds under flood.
    for n in 10..100 {
        let _ignore = queue.push(event(n), 1_200).expect("push");
    }
    assert!(
        queue.take().expect("drain").len() <= 2,
        "the queue never exceeds its bound under sustained flooding"
    );
    // After the quiet window the marker clears and the queue accepts
    // again. The re-flood re-armed the marker at 1_200 (the drops happen
    // on every push past capacity), so the clear is evaluated from the
    // LAST overflow, not the first.
    assert!(
        queue.degraded(1_200 + 299).expect("degraded"),
        "still degraded inside the re-armed flood-free window"
    );
    assert!(
        !queue.degraded(1_200 + 300).expect("degraded"),
        "the degraded marker clears after the flood-free window"
    );
    assert_eq!(
        queue.push(event(200), 1_200 + 300).expect("push"),
        PushOutcome::Accepted,
        "the queue recovers after the quiet window"
    );
}

/// AC 4 (§21.4): rate-limit feedback adapts the poll cadence — a 403
/// with `X-RateLimit-Reset` waits exactly until the reset (never wider),
/// and the durable bookkeeping carries the adapted wait.
#[tokio::test(flavor = "current_thread")]
async fn rate_limit_feedback_adapts_the_cadence_without_widening() {
    let now = 1_000_000_u64;
    let reset = now + 120;
    let server = FixtureServer::start(vec![Rule {
        path_needle: "arbsec/orchestraitor/issues/events",
        requires_etag: None,
        status: 403,
        body: String::new(),
        headers: vec![
            ("X-RateLimit-Remaining", "0".to_string()),
            ("X-RateLimit-Reset", reset.to_string()),
        ],
    }])
    .await;
    let (_queue, store, poller) = fixture_ingress();
    let target = server.target("issues-events", "issues");
    let outcome = poller
        .poll_all(std::slice::from_ref(&target), now)
        .await
        .expect("poll round")
        .remove(0);
    let Err(orchestraitor_daemon::ingress::poller::PollError::RateLimited { retry_after_secs }) =
        outcome
    else {
        panic!("the rate-limited poll must surface the typed RateLimited error");
    };
    assert_eq!(
        retry_after_secs, 120,
        "the wait adapts to exactly the rate-limit reset (never wider)"
    );
    let store = store.lock().expect("store lock");
    assert_eq!(
        store
            .poll_after_secs("arbsec/orchestraitor/issues-events")
            .expect("read"),
        120,
        "the adapted cadence persists for the next poll"
    );
}

/// AC 4 (§21.4): a `Retry-After` hint wins over the reset epoch — the
/// feedback the server explicitly gives is the cadence.
#[tokio::test(flavor = "current_thread")]
async fn retry_after_hint_drives_the_cadence() {
    let now = 1_000_000_u64;
    let server = FixtureServer::start(vec![Rule {
        path_needle: "arbsec/orchestraitor/pulls/events",
        requires_etag: None,
        status: 429,
        body: String::new(),
        headers: vec![("Retry-After", "45".to_string())],
    }])
    .await;
    let (_queue, _store, poller) = fixture_ingress();
    let target = server.target("pulls-events", "pulls");
    let outcome = poller
        .poll_all(std::slice::from_ref(&target), now)
        .await
        .expect("poll round")
        .remove(0);
    assert_eq!(
        outcome,
        Err(
            orchestraitor_daemon::ingress::poller::PollError::RateLimited {
                retry_after_secs: 45
            }
        ),
        "the explicit Retry-After is the adapted wait"
    );
}

/// The event filter: untracked platform verbs never reach the dedup
/// table or the queue (the operator's filter is the fixed §9.47 set).
#[tokio::test(flavor = "current_thread")]
async fn untracked_event_verbs_are_filtered_before_dedup() {
    let body = json!([
        { "id": "333", "event": "assigned", "issue": { "number": 1 } },
        { "id": "444", "event": "closed", "issue": { "number": 2 } }
    ])
    .to_string();
    let server = FixtureServer::start(vec![Rule {
        path_needle: "arbsec/orchestraitor/issues/events",
        requires_etag: None,
        status: 200,
        body,
        headers: vec![("X-Poll-Interval", "60".to_string())],
    }])
    .await;
    let (queue, store, poller) = fixture_ingress();
    let target = server.target("issues-events", "issues");
    let outcome = poller
        .poll_all(std::slice::from_ref(&target), 1_000)
        .await
        .expect("poll round")
        .remove(0)
        .expect("poll ok");
    assert_eq!(
        (outcome.accepted, outcome.duplicates),
        (0, 0),
        "untracked verbs produce nothing"
    );
    assert!(queue.take().expect("drain").is_empty());
    assert!(
        store
            .lock()
            .expect("store lock")
            .record_delivery("333|arbsec/orchestraitor|issue-assigned|", 1_000)
            .expect("record"),
        "the filter never wrote the untracked event's key to the dedup table \
         (a fresh record proves the key was absent)"
    );
}

/// PR events normalize with the head SHA in the delivery key: a new
/// push (new head SHA) on the same PR comment event is a NEW delivery
/// (spec §9.47: head SHA where present is part of the key).
#[tokio::test(flavor = "current_thread")]
async fn pr_events_carry_the_head_sha_in_the_delivery_key() {
    let body = json!([{
        "id": "555",
        "event": "ready_for_review",
        "pull_request": { "number": 504, "head": { "sha": "caad752a" } }
    }])
    .to_string();
    let server = FixtureServer::start(vec![Rule {
        path_needle: "arbsec/orchestraitor/pulls/events",
        requires_etag: None,
        status: 200,
        body,
        headers: vec![("X-Poll-Interval", "60".to_string())],
    }])
    .await;
    let (queue, _store, poller) = fixture_ingress();
    let target = server.target("pulls-events", "pulls");
    let outcome = poller
        .poll_all(std::slice::from_ref(&target), 1_000)
        .await
        .expect("poll round")
        .remove(0)
        .expect("poll ok");
    assert_eq!(outcome.accepted, 1);
    let drained = queue.take().expect("drain");
    let event = &drained[0];
    assert_eq!(event.event_type, "pr-ready-for-review");
    assert_eq!(event.head_sha.as_deref(), Some("caad752a"));
    assert!(
        event.delivery_key.ends_with("|caad752a"),
        "the head SHA rides the delivery key: {}",
        event.delivery_key
    );
}

/// The poll targets are repository-scoped: one issues-events and one
/// pulls-events endpoint PER repository, each keyed by its own repo
/// (§9.47(b): complete over the same event filter; the dedup key and
/// event repo field carry exactly one repository).
#[test]
fn poll_targets_are_repository_scoped() {
    let repos = vec![
        "arbsec/orchestraitor".to_string(),
        "arbsec/arbitraitor".to_string(),
    ];
    let targets = poll_targets(&repos, "https://api.github.com/");
    assert_eq!(targets.len(), 4, "two collections per repository");
    assert!(
        targets
            .iter()
            .all(|t| t.url.starts_with("https://api.github.com/repos/")),
        "every target is repo-scoped: {:?}",
        targets.iter().map(|t| &t.url).collect::<Vec<_>>()
    );
    for repo in &repos {
        assert!(
            targets.iter().any(
                |t| &t.repo == repo && t.url.ends_with(&format!("/repos/{repo}/issues/events"))
            ),
            "an issues-events target exists for {repo}"
        );
        assert!(
            targets.iter().any(|t| &t.repo == repo
                && t.url.ends_with(&format!("/repos/{repo}/pulls/events"))),
            "a pulls-events target exists for {repo}"
        );
    }
    assert_eq!(targets[0].key, "arbsec/orchestraitor/issues-events");
    assert_eq!(targets[1].key, "arbsec/orchestraitor/pulls-events");
    assert_eq!(targets[2].key, "arbsec/arbitraitor/issues-events");
    assert_eq!(targets[3].key, "arbsec/arbitraitor/pulls-events");
}
