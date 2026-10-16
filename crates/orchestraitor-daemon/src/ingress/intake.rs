//! The §9.52(a) bug-report intake attach lane (epic #624, leaf #664):
//! external issue-opened events attach board items with MECHANICAL FIELDS
//! ONLY, never schedule.
//!
//! The trust asymmetry (spec `10-orchestrator.md` §9.52, spine): full-auto
//! ATTACHMENT to the board is safe because every board write is a guarded
//! provider-write through the same path the coordinator tools use (§9.39);
//! full-auto SCHEDULING never is. An intake attach therefore lands the
//! source issue on the configured board at `Status=Triage` — never
//! `Ready` — with `Type=Bug`, a provisional `P1`, and `Target` UNSET
//! (external reports' MVP-ness is unknowable at attach time; the triage
//! classification pass, slice 2, is the only authority that promotes).
//! With `Target` unset the MVP-only ready-queue predicate excludes the
//! item from scheduling by construction: nothing here can cause a spawn.
//!
//! Launch-facts discipline (§9.50(c) via §9.52(a)): every field derives
//! from launch facts ONLY — the author identity and the event type carried
//! by the §9.50 ingress event. Issue titles, bodies, and labels are
//! UNTRUSTED CONTENT (§6.1): they are never read, never parsed, and never
//! allowed to influence a field value, a label, or an attach decision.
//! Author-class provenance labels (`intake:human | intake:bot |
//! intake:agent`) are daemon-owned exactly like the §9.50(c) attribution
//! labels — the filer's own labels confer no authority (§9.52(b)).
//!
//! Idempotency (§9.26.3 via §9.52(a)): one item per source issue. The
//! attach step checks board membership before adding; a re-delivered
//! event updates rather than duplicates. The durable delivery-key dedup
//! in the poller already collapses platform redeliveries; the membership
//! check additionally collapses re-attachment after the local queue
//! drained (e.g. across daemon restarts).
//!
//! Additive posture: an intake outage degrades nothing else — the §9.43
//! reconcile poll and the internal ready queue are untouched, and a
//! disabled lane is a no-op (the shipped default).
//!
//! This crate implements no security primitive: attaching a board item is
//! orchestration bookkeeping, never a trigger authority (spec §9.52); the
//! sandbox boundary remains Arbitraitor's.

use std::sync::Arc;

use crate::ingress::store::IngressEvent;

/// The `[watch.intake]` layered-configuration key prefix (spec §9.52
/// settings; slice 2 adds the classification knobs).
pub const INTAKE_CONFIG_KEY_PREFIX: &str = "watch.intake";

/// Author-class provenance for one intake event (§9.52(a)). Derived from
/// LAUNCH FACTS ONLY: the author's login and the platform's bot-marker
/// association — never from issue content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorClass {
    /// A human account (no bot marker, not in the service-identity set).
    Human,
    /// A bot/automation account (`user.type == "Bot"` or a `Bot`
    /// association the platform stamped).
    Bot,
    /// A principal in the configured service-identity set (§9.41): the
    /// loop's own agent identity. Agent-class issues NEVER attach —
    /// agent-authored reports already flow the internal §9.37/§9.50(c)
    /// paths.
    Agent,
}

impl AuthorClass {
    /// The daemon-owned provenance label value (§9.52(a)).
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Human => "intake:human",
            Self::Bot => "intake:bot",
            Self::Agent => "intake:agent",
        }
    }
}

/// Classifies one author login into its launch-fact author class.
///
/// The classification inputs are launch facts only: the login string and
/// the two trusted configuration facts (the service-identity slugs, and
/// whether the platform marked the author as a bot). Content never
/// participates. A login matching a configured service-identity slug is
/// `Agent`; otherwise a platform-declared bot is `Bot`; otherwise `Human`.
#[must_use]
pub fn classify_author(
    login: &str,
    platform_bot: bool,
    service_identities: &[String],
) -> AuthorClass {
    let normalized = login.trim().to_lowercase();
    let stripped = normalized.strip_suffix("[bot]").unwrap_or(&normalized);
    if service_identities
        .iter()
        .any(|slug| slug.trim().to_lowercase() == stripped)
    {
        return AuthorClass::Agent;
    }
    if platform_bot {
        return AuthorClass::Bot;
    }
    AuthorClass::Human
}

/// The mechanical field set §9.52(a) prescribes for one attached bug
/// report. Every value is a constant of the lane or a launch fact —
/// never content-derived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntakeAttachment {
    /// Repository the source issue lives in (`org/name`).
    pub repo: String,
    /// Source issue number (stable identity: one board item per issue).
    pub number: u64,
    /// The status the item lands at — always `Triage` (§9.52(a): "never
    /// Ready").
    pub status: &'static str,
    /// The native issue type to set — always `Bug` for issue reports.
    pub item_type: &'static str,
    /// The provisional priority (§9.52(d)) — P1, never the scheduling
    /// value.
    pub priority: &'static str,
    /// The daemon-owned author-class provenance label.
    pub label: &'static str,
    /// The author class the label records.
    pub author_class: AuthorClass,
}

/// Derives the mechanical attachment for one accepted §9.52 intake event.
///
/// Returns `None` when the event is not an attachable bug report (a
/// non-`issues-opened` event type, or an `agent`-class author — the loop's
/// own reports keep their existing internal paths and never attach as
/// intake).
#[must_use]
pub fn intake_attachment(
    event: &IngressEvent,
    author_class: AuthorClass,
) -> Option<IntakeAttachment> {
    if event.event_type != "issues-opened" {
        return None;
    }
    if author_class == AuthorClass::Agent {
        return None;
    }
    Some(IntakeAttachment {
        repo: event.repo.clone(),
        number: event.number,
        status: "Triage",
        item_type: "Bug",
        priority: "P1",
        label: author_class.label(),
        author_class,
    })
}

/// The board-write seam the attacher drives. The production
/// implementation routes through the [`orchestraitor_board::BoardClient`]
/// (the same guarded provider-write path the coordinator tools use,
/// §9.39); tests inject hermetic fakes.
#[async_trait::async_trait]
pub trait IntakeBoard: Send + Sync {
    /// Adds the source issue to the board (idempotent per §9.26.3: an
    /// already-attached issue is an update, never a duplicate item).
    ///
    /// # Errors
    ///
    /// Typed transport/GraphQL failures surface to the caller — the tick
    /// retries on the next cadence, never fails open into scheduling.
    async fn attach(
        &self,
        repo: &str,
        number: u64,
        item_type: &str,
        status: &str,
        priority: &str,
        label: &str,
    ) -> Result<bool, String>;
}

/// The production board seam over the shared [`orchestraitor_board::BoardClient`]
/// (the same guarded provider-write path the coordinator tools use, §9.39):
/// `attach` routes into [`orchestraitor_board::BoardClient::attach_issue`]
/// with the §9.52(a) mechanical field set.
pub struct BoardIntake {
    client: Arc<orchestraitor_board::BoardClient>,
    config: orchestraitor_board::BoardProjectConfig,
}

impl BoardIntake {
    /// Builds the seam over the daemon's shared board client and the
    /// board project config the reconcile poll reads.
    #[must_use]
    pub fn new(
        client: Arc<orchestraitor_board::BoardClient>,
        config: orchestraitor_board::BoardProjectConfig,
    ) -> Self {
        Self { client, config }
    }
}

#[async_trait::async_trait]
impl IntakeBoard for BoardIntake {
    async fn attach(
        &self,
        repo: &str,
        number: u64,
        item_type: &str,
        status: &str,
        priority: &str,
        label: &str,
    ) -> Result<bool, String> {
        let fields = orchestraitor_board::IntakeFields {
            item_type,
            status,
            priority,
            label,
        };
        self.client
            .attach_issue(&self.config, repo, number, fields)
            .await
            .map_err(|error| error.to_string())
    }
}

/// The §9.52(a) intake attacher: drains `issues-opened` events from the
/// shared bounded ingress queue and attaches board items through the
/// [`IntakeBoard`] seam.
pub struct IntakeAttacher {
    board: Arc<dyn IntakeBoard>,
    cursor: Box<dyn IntakeCursor>,
    service_identities: Vec<String>,
}

/// The durable intake cursor (the §9.52(a) lane's own consumption
/// record): records every delivery key the lane has FINISHED with —
/// attached, already-attached, skipped, or terminally failed-closed — so
/// a processed event is never re-processed, and the SHARED queue's
/// consumption by the kick-off source never loses an attach. Failed
/// attaches are NOT recorded: they retry on a later tick until the
/// cursor records them.
#[async_trait::async_trait]
pub trait IntakeCursor: Send + Sync {
    /// Filters `keys` down to the ones not yet processed (durable).
    ///
    /// # Errors
    ///
    /// Durable-state failure — the tick refuses to process rather than
    /// risk re-attaching (the board seam's idempotency makes a re-attach
    /// safe, but refusing keeps the failure observable).
    async fn unprocessed(&self, keys: &[String]) -> Result<Vec<String>, String>;

    /// Records keys as processed (durable, idempotent).
    ///
    /// # Errors
    ///
    /// Durable-state failure surfaces to the caller; the keys stay
    /// unrecorded and re-process on the next tick (safe: the board
    /// seam's attach is idempotent).
    async fn record_processed(&self, keys: &[String]) -> Result<(), String>;
}

/// The production cursor: a sqlite table keyed by delivery key. A
/// processed key is never re-processed, across restarts.
pub struct SqliteIntakeCursor {
    conn: std::sync::Mutex<rusqlite::Connection>,
}

impl SqliteIntakeCursor {
    /// Opens (creating if absent) the cursor store at `path`.
    ///
    /// # Errors
    ///
    /// Typed store error when the file cannot be opened or the schema
    /// cannot initialize.
    pub fn open(path: &std::path::Path) -> Result<Self, crate::error::DaemonError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                crate::error::DaemonError::IngressStore {
                    path: path.display().to_string(),
                    reason: format!("parent dir creation failed: {error}"),
                }
            })?;
        }
        let conn = rusqlite::Connection::open(path).map_err(|error| {
            crate::error::DaemonError::IngressStore {
                path: path.display().to_string(),
                reason: format!("sqlite open failed: {error}"),
            }
        })?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS intake_processed (
                 delivery_key    TEXT PRIMARY KEY,
                 processed_secs  INTEGER NOT NULL
             );",
        )
        .map_err(|error| crate::error::DaemonError::IngressStore {
            path: path.display().to_string(),
            reason: format!("schema init failed: {error}"),
        })?;
        Ok(Self {
            conn: std::sync::Mutex::new(conn),
        })
    }
}

#[async_trait::async_trait]
impl IntakeCursor for SqliteIntakeCursor {
    async fn unprocessed(&self, keys: &[String]) -> Result<Vec<String>, String> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| "cursor mutex poisoned".to_string())?;
        let mut pending = Vec::with_capacity(keys.len());
        for key in keys {
            let seen: Option<i64> = conn
                .query_row(
                    "SELECT 1 FROM intake_processed WHERE delivery_key = ?1 LIMIT 1",
                    [key],
                    |row| row.get(0),
                )
                .map(Some)
                .or_else(|error| match error {
                    rusqlite::Error::QueryReturnedNoRows => Ok(None),
                    other => Err(other.to_string()),
                })?;
            if seen.is_none() {
                pending.push(key.clone());
            }
        }
        Ok(pending)
    }

    async fn record_processed(&self, keys: &[String]) -> Result<(), String> {
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs());
        let conn = self
            .conn
            .lock()
            .map_err(|_| "cursor mutex poisoned".to_string())?;
        for key in keys {
            conn.execute(
                "INSERT OR IGNORE INTO intake_processed (delivery_key, processed_secs) \
                 VALUES (?1, ?2)",
                rusqlite::params![key, i64::try_from(now_secs).unwrap_or(i64::MAX)],
            )
            .map_err(|error| error.to_string())?;
        }
        Ok(())
    }
}

impl IntakeAttacher {
    /// Builds the attacher over the board seam, the durable cursor, and
    /// the configured §9.41 service-identity set.
    #[must_use]
    pub fn new(
        board: Arc<dyn IntakeBoard>,
        cursor: Box<dyn IntakeCursor>,
        service_identities: Vec<String>,
    ) -> Self {
        Self {
            board,
            cursor,
            service_identities,
        }
    }

    /// Processes one tick's events: classifies each author from launch
    /// facts, derives the mechanical attachment, and attaches. An event
    /// the durable cursor has already recorded is never re-processed; a
    /// FAILED attach stays unrecorded so it retries on a later tick.
    /// Returns the outcome counts for observability.
    ///
    /// Idempotent by construction: the same event processed twice yields
    /// at most one board item (the cursor, and behind it the board
    /// seam's membership check).
    pub async fn process(&self, events: &[IngressEvent]) -> IntakeOutcome {
        let mut outcome = IntakeOutcome::default();
        // The durable cursor filters first: a key the lane already
        // finished with never re-enters the pipeline (the SHARED queue's
        // consumption by the kick-off source cannot cause a duplicate
        // attach, and a tick with no new work costs no board writes).
        let all_keys: Vec<String> = events
            .iter()
            .map(|event| event.delivery_key.clone())
            .collect();
        let pending_keys = match self.cursor.unprocessed(&all_keys).await {
            Ok(pending) => pending,
            Err(_error) => {
                // Durable-state failure: refuse the whole tick (never
                // risk re-attaching with a cursor that cannot record).
                outcome.failed += events.len();
                return outcome;
            }
        };
        let pending: std::collections::HashSet<&str> =
            pending_keys.iter().map(String::as_str).collect();
        let mut finished: Vec<String> = Vec::new();
        for event in events {
            if !pending.contains(event.delivery_key.as_str()) {
                // Already processed durably — nothing to do.
                continue;
            }
            let Some(login) = event.author_login.as_deref() else {
                // A launch fact the platform did not supply: fail closed
                // to no-attach. The event stays OUT of the board; the
                // §9.43 reconcile poll and a human triage stay the
                // completeness authority. Never guess a class. This is a
                // TERMINAL outcome (re-delivery of the same delivery key
                // would decide identically), so it records.
                outcome.missing_author += 1;
                finished.push(event.delivery_key.clone());
                continue;
            };
            let Some(platform_bot) = event.author_is_bot else {
                outcome.missing_author += 1;
                finished.push(event.delivery_key.clone());
                continue;
            };
            let class = classify_author(login, platform_bot, &self.service_identities);
            let Some(attachment) = intake_attachment(event, class) else {
                // A non-bug-report event type or an agent-class author:
                // terminal (the classification is a pure function of
                // launch facts), so it records.
                outcome.skipped += 1;
                finished.push(event.delivery_key.clone());
                continue;
            };
            match self
                .board
                .attach(
                    &attachment.repo,
                    attachment.number,
                    attachment.item_type,
                    attachment.status,
                    attachment.priority,
                    attachment.label,
                )
                .await
            {
                Ok(attached) => {
                    if attached {
                        outcome.attached += 1;
                    } else {
                        outcome.already_attached += 1;
                    }
                    finished.push(event.delivery_key.clone());
                }
                Err(_error) => {
                    // Log-safe failure: identifiers only, never content.
                    // The key stays UNRECORDED — the next tick retries
                    // the attach (the board seam's membership check
                    // makes a re-attach after a partial write safe).
                    outcome.failed += 1;
                }
            }
        }
        if !finished.is_empty()
            && let Err(_error) = self.cursor.record_processed(&finished).await
        {
            // The record failure means those keys re-process next tick —
            // safe (the board attach is idempotent), so the tick still
            // reports its counts; the failure is counted for visibility.
            outcome.failed += finished.len();
        }
        outcome
    }
}

/// One attach tick's observability counts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IntakeOutcome {
    /// Board items newly attached.
    pub attached: usize,
    /// Events whose source issue was already on the board (idempotent
    /// update, not a duplicate).
    pub already_attached: usize,
    /// Events skipped without attaching (non-bug-report event types,
    /// agent-class authors).
    pub skipped: usize,
    /// Events with no author launch fact: failed closed, never guessed.
    pub missing_author: usize,
    /// Board-write failures: retried on a later tick, never fatal.
    pub failed: usize,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    // Test-only allowances mirror the crate's other unit-test modules:
    // a failed assertion must fail the test loudly.

    use super::*;

    /// A recording board seam: every attach is captured; the outcome is
    /// scripted per call. `Clone` shares the call log (an `Arc` inside),
    /// so a test can hold a clone while the attacher moves another.
    /// One recorded `attach` call: (repo, number, type, status,
    /// priority, label).
    type RecordedCall = (String, u64, String, String, String, String);

    #[derive(Clone)]
    struct RecordingBoard {
        /// Scripted outcomes, popped front-to-back; an exhausted script
        /// answers `Ok(true)` (the common path).
        outcomes: std::sync::Arc<std::sync::Mutex<Vec<Result<bool, String>>>>,
        calls: std::sync::Arc<std::sync::Mutex<Vec<RecordedCall>>>,
    }

    impl RecordingBoard {
        fn new(outcomes: Vec<Result<bool, String>>) -> Self {
            Self {
                outcomes: std::sync::Arc::new(std::sync::Mutex::new(outcomes)),
                calls: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            }
        }

        fn calls(&self) -> Vec<RecordedCall> {
            self.calls.lock().expect("calls lock").clone()
        }
    }

    /// An in-memory cursor: the same contract as the sqlite cursor.
    #[derive(Clone, Default)]
    struct MemoryCursor {
        processed: std::sync::Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
        fail_unprocessed: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }

    impl MemoryCursor {
        fn recorded(&self) -> Vec<String> {
            self.processed
                .lock()
                .expect("cursor lock")
                .iter()
                .cloned()
                .collect()
        }
    }

    #[async_trait::async_trait]
    impl IntakeCursor for MemoryCursor {
        async fn unprocessed(&self, keys: &[String]) -> Result<Vec<String>, String> {
            if self
                .fail_unprocessed
                .load(std::sync::atomic::Ordering::SeqCst)
            {
                return Err(String::from("cursor unavailable"));
            }
            let seen = self.processed.lock().expect("cursor lock");
            Ok(keys
                .iter()
                .filter(|key| !seen.contains(*key))
                .cloned()
                .collect())
        }

        async fn record_processed(&self, keys: &[String]) -> Result<(), String> {
            let mut seen = self.processed.lock().expect("cursor lock");
            for key in keys {
                seen.insert(key.clone());
            }
            Ok(())
        }
    }

    #[async_trait::async_trait]
    impl IntakeBoard for RecordingBoard {
        async fn attach(
            &self,
            repo: &str,
            number: u64,
            item_type: &str,
            status: &str,
            priority: &str,
            label: &str,
        ) -> Result<bool, String> {
            self.calls.lock().expect("calls lock").push((
                repo.to_string(),
                number,
                item_type.to_string(),
                status.to_string(),
                priority.to_string(),
                label.to_string(),
            ));
            let mut outcomes = self.outcomes.lock().expect("outcomes lock");
            if outcomes.is_empty() {
                return Ok(true);
            }
            outcomes.remove(0)
        }
    }

    fn event(
        number: u64,
        event_type: &str,
        login: Option<&str>,
        bot: Option<bool>,
    ) -> IngressEvent {
        IngressEvent {
            delivery_key: format!("key-{number}"),
            repo: "arbsec/orchestraitor".to_string(),
            event_type: event_type.to_string(),
            head_sha: None,
            number,
            author_login: login.map(std::string::ToString::to_string),
            author_is_bot: bot,
            accepted_at_secs: 1_000,
        }
    }

    /// AC 1 (§9.52(a)): a human-authored issues-opened event attaches with
    /// the exact mechanical field set — Triage, Bug, P1, the intake:human
    /// label.
    #[tokio::test]
    async fn human_bug_report_attaches_with_mechanical_fields() {
        let board = RecordingBoard::new(vec![]);
        let attacher = IntakeAttacher::new(
            std::sync::Arc::new(board.clone()),
            Box::new(MemoryCursor::default()),
            vec![String::from("arbsec-agent")],
        );
        let outcome = attacher
            .process(&[event(700, "issues-opened", Some("somehuman"), Some(false))])
            .await;
        assert_eq!(outcome.attached, 1);
        assert_eq!(outcome.skipped, 0);
        let calls = board.calls();
        assert_eq!(calls.len(), 1, "exactly one board write");
        let (repo, number, item_type, status, priority, label) = &calls[0];
        assert_eq!(repo, "arbsec/orchestraitor");
        assert_eq!(*number, 700);
        assert_eq!(item_type, "Bug");
        assert_eq!(status, "Triage", "the trust asymmetry: never Ready");
        assert_eq!(
            priority, "P1",
            "the provisional priority, never the scheduling value"
        );
        assert_eq!(label, "intake:human");
    }

    /// AC 1 (§9.52(a)): a bot author attaches with the intake:bot label.
    #[tokio::test]
    async fn bot_author_attaches_with_bot_label() {
        let board = RecordingBoard::new(vec![]);
        let attacher = IntakeAttacher::new(
            std::sync::Arc::new(board.clone()),
            Box::new(MemoryCursor::default()),
            vec![String::from("arbsec-agent")],
        );
        let outcome = attacher
            .process(&[event(701, "issues-opened", Some("renovate"), Some(true))])
            .await;
        assert_eq!(outcome.attached, 1);
        let (_, _, _, _, _, label) = &board.calls()[0];
        assert_eq!(label, "intake:bot");
    }

    /// A service-identity author NEVER attaches (the loop's own reports
    /// flow the internal paths) — negative test.
    #[tokio::test]
    async fn service_identity_author_never_attaches() {
        let board = RecordingBoard::new(vec![]);
        let attacher = IntakeAttacher::new(
            std::sync::Arc::new(board.clone()),
            Box::new(MemoryCursor::default()),
            vec![String::from("arbsec-agent")],
        );
        let outcome = attacher
            .process(&[event(
                702,
                "issues-opened",
                Some("arbsec-agent"),
                Some(false),
            )])
            .await;
        assert_eq!(outcome.attached, 0);
        assert_eq!(outcome.skipped, 1, "agent-class events skip the attach");
        assert!(board.calls().is_empty());
    }

    /// The service-identity match is slug-normalized: `Arbsec-Agent[bot]`
    /// resolves to the agent class.
    #[test]
    fn classify_author_matches_slug_case_and_bot_suffix() {
        assert_eq!(
            classify_author("Arbsec-Agent[bot]", false, &[String::from("arbsec-agent")],),
            AuthorClass::Agent
        );
        assert_eq!(
            classify_author("renovate", true, &[String::from("arbsec-agent")]),
            AuthorClass::Bot
        );
        assert_eq!(
            classify_author("somehuman", false, &[String::from("arbsec-agent")]),
            AuthorClass::Human
        );
    }

    /// Non-bug-report event types never attach (the lane is the
    /// issues-opened consumer only).
    #[tokio::test]
    async fn non_opened_event_types_skip() {
        let board = RecordingBoard::new(vec![]);
        let attacher = IntakeAttacher::new(
            std::sync::Arc::new(board.clone()),
            Box::new(MemoryCursor::default()),
            vec![String::from("arbsec-agent")],
        );
        let outcome = attacher
            .process(&[
                event(703, "issue-commented", Some("somehuman"), Some(false)),
                event(704, "pr-commented", Some("somehuman"), Some(false)),
            ])
            .await;
        assert_eq!(outcome.attached, 0);
        assert_eq!(outcome.skipped, 2);
        assert!(board.calls().is_empty());
    }

    /// AC 3 (§21.4 posture): an event with NO author launch fact fails
    /// closed — no board write, no guessed class.
    #[tokio::test]
    async fn missing_author_fact_fails_closed() {
        let board = RecordingBoard::new(vec![]);
        let attacher = IntakeAttacher::new(
            std::sync::Arc::new(board.clone()),
            Box::new(MemoryCursor::default()),
            vec![String::from("arbsec-agent")],
        );
        let outcome = attacher
            .process(&[event(705, "issues-opened", None, None)])
            .await;
        assert_eq!(outcome.attached, 0);
        assert_eq!(outcome.missing_author, 1);
        assert!(board.calls().is_empty());
    }

    /// AC 2 (§9.26.3): the already-attached answer is the idempotent
    /// update path — counted, never a duplicate write.
    #[tokio::test]
    async fn re_delivery_updates_rather_than_duplicates() {
        let board = RecordingBoard::new(vec![Ok(false)]);
        let attacher = IntakeAttacher::new(
            std::sync::Arc::new(board.clone()),
            Box::new(MemoryCursor::default()),
            vec![String::from("arbsec-agent")],
        );
        let outcome = attacher
            .process(&[
                event(706, "issues-opened", Some("somehuman"), Some(false)),
                event(706, "issues-opened", Some("somehuman"), Some(false)),
            ])
            .await;
        assert_eq!(outcome.attached, 1, "the first delivery attaches");
        assert_eq!(
            outcome.already_attached, 1,
            "the redelivery takes the update path"
        );
    }

    /// A board-write failure is counted and retried later — never fatal,
    /// never fail-open.
    #[tokio::test]
    async fn board_write_failure_is_counted_not_fatal() {
        let board = RecordingBoard::new(vec![Err(String::from("transport down"))]);
        let attacher = IntakeAttacher::new(
            std::sync::Arc::new(board.clone()),
            Box::new(MemoryCursor::default()),
            vec![String::from("arbsec-agent")],
        );
        let outcome = attacher
            .process(&[event(707, "issues-opened", Some("somehuman"), Some(false))])
            .await;
        assert_eq!(outcome.attached, 0);
        assert_eq!(outcome.failed, 1);
    }

    /// Gen-1 review fix: a processed event is NEVER re-processed on a
    /// later tick (the durable cursor records terminal outcomes), and a
    /// FAILED attach stays unrecorded so it retries.
    #[tokio::test]
    async fn cursor_prevents_reprocessing_and_failed_attaches_retry() {
        // First tick: the attach FAILS (board down).
        let board = RecordingBoard::new(vec![Err(String::from("transport down"))]);
        let cursor = MemoryCursor::default();
        let attacher = IntakeAttacher::new(
            std::sync::Arc::new(board.clone()),
            Box::new(cursor.clone()),
            vec![String::from("arbsec-agent")],
        );
        let event = event(710, "issues-opened", Some("somehuman"), Some(false));
        let outcome = attacher.process(std::slice::from_ref(&event)).await;
        assert_eq!(outcome.failed, 1, "the attach failed");
        assert_eq!(
            cursor.recorded().len(),
            0,
            "a failed attach stays unrecorded"
        );

        // Second tick: the attach SUCCEEDS — the event retried.
        let outcome = attacher.process(std::slice::from_ref(&event)).await;
        assert_eq!(outcome.attached, 1);
        assert_eq!(cursor.recorded(), vec![event.delivery_key.clone()]);

        // Third tick: the same delivery (e.g. the kick-off source never
        // drained it) does NOT re-attach.
        let outcome = attacher.process(std::slice::from_ref(&event)).await;
        assert_eq!(
            (outcome.attached, outcome.already_attached, outcome.skipped),
            (0, 0, 0),
            "a cursor-recorded event is never re-processed"
        );
        assert_eq!(board.calls().len(), 2, "exactly two board writes total");
    }

    /// Gen-1 review fix: terminal classifications (missing author facts,
    /// skipped events) record — they never re-process nor re-log.
    #[tokio::test]
    async fn terminal_outcomes_record_once() {
        let board = RecordingBoard::new(vec![]);
        let cursor = MemoryCursor::default();
        let attacher = IntakeAttacher::new(
            std::sync::Arc::new(board),
            Box::new(cursor.clone()),
            vec![String::from("arbsec-agent")],
        );
        let missing = event(711, "issues-opened", None, None);
        let skipped = event(712, "issue-commented", Some("somehuman"), Some(false));
        // Tick 1: both terminal outcomes fire and record.
        let outcome = attacher.process(&[missing.clone(), skipped.clone()]).await;
        assert_eq!(outcome.missing_author, 1);
        assert_eq!(outcome.skipped, 1);
        assert_eq!(cursor.recorded().len(), 2);
        // Ticks 2 and 3: the same deliveries neither re-log nor re-record.
        for _ in 0..2 {
            let outcome = attacher.process(&[missing.clone(), skipped.clone()]).await;
            assert_eq!(
                (outcome.missing_author, outcome.skipped, outcome.attached),
                (0, 0, 0),
                "a recorded terminal outcome is silent on re-delivery"
            );
        }
        assert_eq!(
            cursor.recorded().len(),
            2,
            "both terminal outcomes recorded exactly once"
        );
    }

    /// A cursor READ failure refuses the whole tick (never re-attaches
    /// with a cursor that cannot record).
    #[tokio::test]
    async fn cursor_read_failure_refuses_the_tick() {
        let board = RecordingBoard::new(vec![]);
        let cursor = MemoryCursor::default();
        cursor
            .fail_unprocessed
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let attacher = IntakeAttacher::new(
            std::sync::Arc::new(board.clone()),
            Box::new(cursor),
            vec![String::from("arbsec-agent")],
        );
        let outcome = attacher
            .process(&[event(713, "issues-opened", Some("somehuman"), Some(false))])
            .await;
        assert_eq!(outcome.failed, 1);
        assert!(
            board.calls().is_empty(),
            "no board write happens when the cursor cannot read"
        );
    }

    /// The production sqlite cursor round-trips (open → filter → record
    /// → filter) across a fresh open (restart persistence).
    #[tokio::test]
    async fn sqlite_cursor_persists_across_reopen() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("intake-cursor.db");
        {
            let cursor = SqliteIntakeCursor::open(&path).expect("open");
            cursor
                .record_processed(&[String::from("key-a")])
                .await
                .expect("record");
        }
        let cursor = SqliteIntakeCursor::open(&path).expect("reopen");
        let pending = cursor
            .unprocessed(&[String::from("key-a"), String::from("key-b")])
            .await
            .expect("filter");
        assert_eq!(pending, vec![String::from("key-b")]);
    }
}
