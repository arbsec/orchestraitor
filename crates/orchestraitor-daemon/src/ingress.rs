//! The §9.50 GitHub event ingress (issue #504): the poll-only fallback
//! that feeds external GitHub activity (new issues, issue/PR comments)
//! into the §9.36 kick-off evaluation. PR ready-for-review is planned
//! but not yet tracked: the repository events feed this poller consumes
//! does not carry that event.
//!
//! - [`poller::EventPoller`] — conditional requests (ETag/304) over the
//!   REST API, honoring `X-Poll-Interval` and rate-limit feedback
//!   (§9.43); the cadence never widens beyond that feedback.
//! - [`store::IngressStore`] — the durable dedup table (delivery key =
//!   platform delivery id + repo + event type + head SHA, 30-day
//!   redelivery window) and the per-endpoint ETag bookkeeping.
//! - [`queue::IngressQueue`] — the bounded ingress queue: sustained
//!   flooding drops surplus and records the `degraded` marker, never
//!   unbounded growth (§7.3); the marker clears only after a flood-free
//!   window.
//! - [`KickoffIngressSource`] — the production
//!   `orchestraitor_campaign::KickoffEventSource` over the queue, so the
//!   operator can arm the external-event kick-off condition
//!   (`[watch.kickoff].external_event`).
//!
//! The push relay (§9.50(a)) is intentionally absent: it stays
//! `blocked:arbitraitor` until ingress-envelope authentication exists
//! (arbsec/arbitraitor#787). Poll-only is complete ingress per §9.50(b);
//! the §9.43 reconcile poll stays the completeness authority either way.
//!
//! This crate implements no security primitive: event ingestion is
//! orchestration acceleration, never a second trigger authority (spec
//! §9.50); the sandbox boundary remains Arbitraitor's.

pub mod ci_lane;
pub mod intake;
pub mod poller;
pub mod queue;
pub mod store;

use std::sync::Arc;

use orchestraitor_campaign::KickoffExternalEvent;

use crate::error::DaemonError;
use crate::ingress::queue::IngressQueue;

/// The production §9.50 external-event source (issue #504): drains the
/// bounded ingress queue into the kick-off seam. Events carry
/// identifiers only — never board or PR content.
#[derive(Debug, Clone)]
pub struct KickoffIngressSource {
    queue: Arc<IngressQueue>,
}

impl KickoffIngressSource {
    /// Builds the source over the shared bounded queue.
    #[must_use]
    pub fn new(queue: Arc<IngressQueue>) -> Self {
        Self { queue }
    }
}

impl orchestraitor_campaign::KickoffEventSource for KickoffIngressSource {
    fn pending(&self) -> Vec<KickoffExternalEvent> {
        // A PEEK: non-destructive (the kick-off gate evaluates against
        // pending events; the destructive drain runs only after the
        // pass's planner selects a task — a gen-6 cloud review fix).
        self.queue
            .peek()
            .into_iter()
            .map(|event| KickoffExternalEvent {
                id: event.delivery_key,
            })
            .collect()
    }

    fn drain_pending(&self, ids: &[String]) -> Vec<KickoffExternalEvent> {
        // Remove EXACTLY the evaluated ids; events that arrived after
        // the peek stay pending (a gen-9 review fix: a drain-everything
        // would lose the late arrivals' kick-off acceleration).
        self.queue
            .take_by_ids(ids)
            .into_iter()
            .map(|event| KickoffExternalEvent {
                id: event.delivery_key,
            })
            .collect()
    }
}

/// Convenience constructor: opens the durable store, builds the bounded
/// queue, and wires the poller + kick-off source over them.
///
/// # Errors
///
/// Returns [`DaemonError::IngressStore`] or
/// [`DaemonError::IngressHttp`] when a component cannot be built, and
/// [`DaemonError::WatchConfig`] for a zero queue capacity.
pub fn build_ingress(
    store_path: &std::path::Path,
    auth: Arc<dyn orchestraitor_board::BoardAuth>,
    queue_capacity: usize,
    intake_store: Option<Arc<crate::ingress::intake::SqliteIntakeStore>>,
) -> Result<(Arc<IngressQueue>, poller::EventPoller, KickoffIngressSource), DaemonError> {
    let store = store::IngressStore::open(store_path)?;
    let queue = Arc::new(queue::IngressQueue::new(queue_capacity)?);
    let poller = poller::EventPoller::with_intake(
        auth,
        Arc::new(std::sync::Mutex::new(store)),
        Arc::clone(&queue),
        intake_store.map(|store| store as Arc<dyn crate::ingress::intake::IntakeStore>),
    )?;
    let source = KickoffIngressSource::new(Arc::clone(&queue));
    Ok((queue, poller, source))
}
