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
    service_identities: Vec<String>,
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

    /// Processes one tick's events: classifies each author from launch
    /// facts, derives the mechanical attachment, and attaches. Returns
    /// the number of items attached (and skipped counts for observability).
    ///
    /// Idempotent by construction: the same event processed twice yields
    /// at most one board item (the board seam's membership check).
    pub async fn process(&self, events: &[IngressEvent]) -> IntakeOutcome {
        let mut outcome = IntakeOutcome::default();
        for event in events {
            let Some(login) = event.author_login.as_deref() else {
                // A launch fact the platform did not supply: fail closed
                // to no-attach. The event stays OUT of the board; the
                // §9.43 reconcile poll and a human triage stay the
                // completeness authority. Never guess a class.
                outcome.missing_author += 1;
                continue;
            };
            let Some(platform_bot) = event.author_is_bot else {
                outcome.missing_author += 1;
                continue;
            };
            let class = classify_author(login, platform_bot, &self.service_identities);
            let Some(attachment) = intake_attachment(event, class) else {
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
                    // The item does not attach this tick; the next
                    // delivery or reconcile tick retries.
                    outcome.failed += 1;
                }
            }
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
}
