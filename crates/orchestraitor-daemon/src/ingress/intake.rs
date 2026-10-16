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
    classify_author_opt(login, Some(platform_bot), service_identities)
}

/// The optional-bot-fact variant: `None` (the platform supplied no bot
/// fact) classifies conservatively as a bot AFTER the service-identity
/// check — the trust asymmetry prefers mislabeling a human as a bot
/// over the reverse, and the label is provenance, never a scheduling
/// value, so the report still attaches.
#[must_use]
pub fn classify_author_opt(
    login: &str,
    platform_bot: Option<bool>,
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
    // A `[bot]`-suffixed login is itself a launch fact: the platform's
    // login convention marks automation regardless of the user-type
    // field.
    if stripped != normalized {
        return AuthorClass::Bot;
    }
    if platform_bot.unwrap_or(true) {
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

/// The §9.52(a) intake attacher: consumes `issues-opened` events taken
/// from the durable intake store and attaches board items through the
/// [`IntakeBoard`] seam.
pub struct IntakeAttacher {
    board: Arc<dyn IntakeBoard>,
    service_identities: Vec<String>,
}

/// One attach tick's observability counts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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
    /// The events whose attach FAILED (exactly the events the caller
    /// must re-put for the next tick's retry — never the attached,
    /// skipped, or terminal events).
    pub failed_events: Vec<IngressEvent>,
}

/// The durable intake store (the §9.52(a) lane's own consumption
/// queue, decoupled from the SHARED in-memory ingress queue): the
/// poller's accept path writes every accepted `issues-opened` event
/// here, and the lane consumes with delete-on-finish. An event the
/// kick-off source drains from the shared queue FIRST is still here —
/// the two consumers cannot steal from each other, a dropped
/// queue-surplus event is not lost (the store holds it), and a daemon
/// restart re-attaches from durable state instead of memory.
///
/// A FINISHED event (attached, already-attached, skipped, or terminally
/// failed-closed) is deleted; a FAILED attach stays in the store and
/// retries on a later tick. Every board write is idempotent behind the
/// board seam's membership check, so even an interrupted tick cannot
/// duplicate an item.
#[async_trait::async_trait]
pub trait IntakeStore: Send + Sync {
    /// Stores one accepted event (idempotent per delivery key). Called
    /// on the poller's accept path.
    ///
    /// # Errors
    ///
    /// Durable-state failure — surfaced to the caller, never swallowed.
    async fn put(&self, event: &IngressEvent) -> Result<(), String>;

    /// Takes up to `limit` stored events (durable, delete-on-read):
    /// each taken event leaves the store, so a taken event is processed
    /// exactly once. A caller that fails to finish an event re-`put`s
    /// it.
    ///
    /// # Errors
    ///
    /// Durable-state failure — the tick refuses to process rather than
    /// process nothing silently.
    async fn take(&self, limit: usize) -> Result<Vec<IngressEvent>, String>;
}

/// The production store: a sqlite table keyed by delivery key, deleted
/// on take. A daemon restart re-attaches from durable state.
pub struct SqliteIntakeStore {
    conn: std::sync::Mutex<rusqlite::Connection>,
}

impl SqliteIntakeStore {
    /// Opens (creating if absent) the intake store at `path`.
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
             CREATE TABLE IF NOT EXISTS intake_events (
                 delivery_key   TEXT PRIMARY KEY,
                 repo           TEXT NOT NULL,
                 number         INTEGER NOT NULL,
                 author_login   TEXT,
                 author_is_bot  INTEGER,
                 accepted_secs  INTEGER NOT NULL
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

    /// Synchronous put (the async trait wraps this).
    fn put_sync(&self, event: &IngressEvent) -> Result<(), String> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| "store mutex poisoned".to_string())?;
        conn.execute(
            "INSERT OR IGNORE INTO intake_events \
             (delivery_key, repo, number, author_login, author_is_bot, accepted_secs) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                event.delivery_key,
                event.repo,
                i64::try_from(event.number).unwrap_or(i64::MAX),
                event.author_login,
                event.author_is_bot.map(i64::from),
                i64::try_from(event.accepted_at_secs).unwrap_or(i64::MAX),
            ],
        )
        .map_err(|error| error.to_string())?;
        Ok(())
    }

    /// Synchronous take (the async trait wraps this): reads up to
    /// `limit` rows oldest-first and deletes exactly those rows in the
    /// same transaction.
    fn take_sync(&self, limit: usize) -> Result<Vec<IngressEvent>, String> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| "store mutex poisoned".to_string())?;
        let tx = conn.transaction().map_err(|error| error.to_string())?;
        let mut statement = tx
            .prepare(
                "SELECT delivery_key, repo, number, author_login, author_is_bot, accepted_secs \
                 FROM intake_events ORDER BY accepted_secs, delivery_key LIMIT ?1",
            )
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map([i64::try_from(limit).unwrap_or(i64::MAX)], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            })
            .map_err(|error| error.to_string())?;
        let mut events = Vec::new();
        for row in rows {
            let (delivery_key, repo, number, author_login, author_is_bot, accepted_secs) =
                row.map_err(|error| error.to_string())?;
            events.push(IngressEvent {
                delivery_key,
                repo,
                number: u64::try_from(number).unwrap_or(u64::MAX),
                event_type: String::from("issues-opened"),
                head_sha: None,
                author_login,
                author_is_bot: author_is_bot.map(|bot| bot != 0),
                accepted_at_secs: u64::try_from(accepted_secs).unwrap_or(u64::MAX),
            });
        }
        drop(statement);
        for event in &events {
            tx.execute(
                "DELETE FROM intake_events WHERE delivery_key = ?1",
                [&event.delivery_key],
            )
            .map_err(|error| error.to_string())?;
        }
        tx.commit().map_err(|error| error.to_string())?;
        Ok(events)
    }
}

#[async_trait::async_trait]
impl IntakeStore for SqliteIntakeStore {
    async fn put(&self, event: &IngressEvent) -> Result<(), String> {
        self.put_sync(event)
    }

    async fn take(&self, limit: usize) -> Result<Vec<IngressEvent>, String> {
        self.take_sync(limit)
    }
}

impl IntakeAttacher {
    /// Builds the attacher over the board seam and the configured
    /// §9.41 service-identity set.
    #[must_use]
    pub fn new(board: Arc<dyn IntakeBoard>, service_identities: Vec<String>) -> Self {
        Self {
            board,
            service_identities,
        }
    }

    /// Processes one tick's events (taken from the durable intake
    /// store): classifies each author from launch facts, derives the
    /// mechanical attachment, and attaches. Returns the outcome counts
    /// for observability.
    ///
    /// Idempotent by construction: the board seam's membership check
    /// yields at most one board item per source issue, so even a tick
    /// interrupted mid-way cannot duplicate.
    pub async fn process(&self, events: &[IngressEvent]) -> IntakeOutcome {
        let mut outcome = IntakeOutcome::default();
        for event in events {
            // The launch-fact contract (§9.50(c) via §9.52(a)): the
            // LOGIN is required — without it there is no author fact at
            // all and the event fails closed (terminal: a re-delivery
            // would decide identically). The BOT fact is optional: an
            // unknown bot fact classifies conservatively as a bot AFTER
            // the service-identity check (the trust asymmetry prefers
            // mislabeling a human as a bot over the reverse; the label
            // is provenance, never a scheduling value) — the event
            // still attaches (gen-3 review fix: an unknown bot fact
            // from an actor-only event must not drop the report).
            let Some(login) = event.author_login.as_deref() else {
                outcome.missing_author += 1;
                continue;
            };
            let class = classify_author_opt(login, event.author_is_bot, &self.service_identities);
            let Some(attachment) = intake_attachment(event, class) else {
                // A non-bug-report event type or an agent-class author:
                // the classification is a pure function of launch facts.
                outcome.skipped += 1;
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
                }
                Err(_error) => {
                    // Log-safe failure: identifiers only, never content.
                    // The event is returned in `failed_events` so the
                    // caller re-puts EXACTLY the failed batch (never the
                    // attached, skipped, or terminal events) and the
                    // next tick retries the attach. The repair policy in
                    // the board seam makes a re-attach after a partial
                    // write complete the item (unset fields are
                    // written; set fields are left alone).
                    outcome.failed += 1;
                    outcome.failed_events.push(event.clone());
                }
            }
        }
        outcome
    }
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

    /// An in-memory intake store: the same contract as the sqlite
    /// store (put is idempotent per key; take is delete-on-read).
    #[derive(Clone, Default)]
    struct MemoryStore {
        events: std::sync::Arc<std::sync::Mutex<Vec<IngressEvent>>>,
        fail_take: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }

    impl MemoryStore {
        fn len(&self) -> usize {
            self.events.lock().expect("store lock").len()
        }
    }

    #[async_trait::async_trait]
    impl IntakeStore for MemoryStore {
        async fn put(&self, event: &IngressEvent) -> Result<(), String> {
            let mut events = self.events.lock().expect("store lock");
            if events.iter().any(|e| e.delivery_key == event.delivery_key) {
                return Ok(());
            }
            events.push(event.clone());
            Ok(())
        }

        async fn take(&self, limit: usize) -> Result<Vec<IngressEvent>, String> {
            if self.fail_take.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(String::from("store unavailable"));
            }
            let mut events = self.events.lock().expect("store lock");
            let take_count = limit.min(events.len());
            let taken: Vec<IngressEvent> = events.drain(..take_count).collect();
            Ok(taken)
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
            vec![String::from("arbsec-agent")],
        );
        let outcome = attacher
            .process(&[event(707, "issues-opened", Some("somehuman"), Some(false))])
            .await;
        assert_eq!(outcome.attached, 0);
        assert_eq!(outcome.failed, 1);
    }

    /// Gen-2 review fix (durable intake store): a FAILED attach is
    /// re-put by the caller and retried on the next tick — the take
    /// deleted it, so the retry path is re-put → take → process.
    #[tokio::test]
    async fn failed_attach_is_reput_and_retried() {
        let board = RecordingBoard::new(vec![Err(String::from("transport down"))]);
        let attacher = IntakeAttacher::new(
            std::sync::Arc::new(board.clone()),
            vec![String::from("arbsec-agent")],
        );
        let store = MemoryStore::default();
        let event = event(710, "issues-opened", Some("somehuman"), Some(false));
        store.put(&event).await.expect("put");

        // Tick 1: the attach FAILS.
        let taken = store.take(8).await.expect("take");
        let outcome = attacher.process(&taken).await;
        assert_eq!(outcome.failed, 1, "the attach failed");
        assert_eq!(store.len(), 0, "the take deleted the event");
        assert_eq!(board.calls().len(), 1, "one attempted board write");

        // The loop re-puts the failed batch.
        for event in &taken {
            store.put(event).await.expect("re-put");
        }
        // Tick 2: the attach SUCCEEDS (board healed).
        let board2 = RecordingBoard::new(vec![]);
        let attacher2 = IntakeAttacher::new(
            std::sync::Arc::new(board2),
            vec![String::from("arbsec-agent")],
        );
        let taken = store.take(8).await.expect("take");
        let outcome = attacher2.process(&taken).await;
        assert_eq!(outcome.attached, 1);
        assert_eq!(store.len(), 0, "the finished event stays deleted");
    }

    /// The durable store's take is delete-on-read: an event taken by
    /// one tick is NEVER handed to another (exactly-once consumption).
    #[tokio::test]
    async fn store_take_is_delete_on_read() {
        let store = MemoryStore::default();
        store
            .put(&event(713, "issues-opened", Some("a"), Some(false)))
            .await
            .expect("put");
        store
            .put(&event(714, "issues-opened", Some("b"), Some(false)))
            .await
            .expect("put");
        let first = store.take(8).await.expect("take");
        assert_eq!(first.len(), 2);
        let second = store.take(8).await.expect("take");
        assert!(
            second.is_empty(),
            "taken events leave the store — no double consumption"
        );
        // Re-put of the same delivery key is idempotent while held, and
        // after take it re-enters as a fresh event (the board seam's
        // membership check makes the re-attach safe).
        store.put(&first[0]).await.expect("re-put");
        assert_eq!(store.len(), 1);
    }

    /// A store READ failure surfaces to the loop (which logs and
    /// retries); the lane never processes nothing silently.
    #[tokio::test]
    async fn store_read_failure_surfaces() {
        let store = MemoryStore::default();
        store
            .fail_take
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let error = store.take(8).await.expect_err("the read fails");
        assert_eq!(error, "store unavailable");
    }

    /// The production sqlite intake store round-trips put → take with
    /// delete-on-take, and persists across a fresh open (restart
    /// durability — a taken-but-unprocessed event survives... no: a
    /// TAKEN event is deleted; an UNTAKEN event persists).
    #[tokio::test]
    async fn sqlite_store_persists_untaken_events_across_reopen() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("intake-events.db");
        {
            let store = SqliteIntakeStore::open(&path).expect("open");
            store
                .put(&event(715, "issues-opened", Some("a"), Some(false)))
                .await
                .expect("put");
            store
                .put(&event(716, "issues-opened", Some("b"), Some(true)))
                .await
                .expect("put");
        }
        let store = SqliteIntakeStore::open(&path).expect("reopen");
        let taken = store.take(8).await.expect("take");
        assert_eq!(taken.len(), 2, "untaken events persist across restart");
        assert_eq!(taken[0].author_login.as_deref(), Some("a"));
        assert_eq!(taken[1].author_is_bot, Some(true));
        let again = store.take(8).await.expect("take");
        assert!(again.is_empty(), "delete-on-take");
    }
}
