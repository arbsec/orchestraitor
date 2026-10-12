//! The §9.47 GitHub event ingress (issue #504): the poll-only fallback
//! that feeds external GitHub activity (new issues, issue/PR comments,
//! PR ready-for-review) into the §9.36 kick-off evaluation.
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
//! The push relay (§9.47(a)) is intentionally absent: it stays
//! `blocked:arbitraitor` until ingress-envelope authentication exists
//! (arbsec/arbitraitor#787). Poll-only is complete ingress per §9.47(b);
//! the §9.43 reconcile poll stays the completeness authority either way.
//!
//! This crate implements no security primitive: event ingestion is
//! orchestration acceleration, never a second trigger authority (spec
//! §9.47); the sandbox boundary remains Arbitraitor's.

pub mod poller;
pub mod queue;
pub mod store;

use std::sync::Arc;

use orchestraitor_campaign::KickoffExternalEvent;

use crate::error::DaemonError;
use crate::ingress::queue::IngressQueue;

/// The production §9.47 external-event source (issue #504): drains the
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
        match self.queue.peek() {
            Ok(events) => events
                .into_iter()
                .map(|event| KickoffExternalEvent {
                    id: event.delivery_key,
                })
                .collect(),
            // A poisoned queue is an internal defect; the kick-off gate
            // must see an empty source rather than panic inside the loop
            // runner — the queue's own error path surfaces it on the next
            // push/read, and the §9.43 reconcile poll keeps completeness.
            Err(_) => Vec::new(),
        }
    }

    fn drain_pending(&self) -> Vec<KickoffExternalEvent> {
        match self.queue.take() {
            Ok(events) => events
                .into_iter()
                .map(|event| KickoffExternalEvent {
                    id: event.delivery_key,
                })
                .collect(),
            Err(_) => Vec::new(),
        }
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
) -> Result<(Arc<IngressQueue>, poller::EventPoller, KickoffIngressSource), DaemonError> {
    let store = store::IngressStore::open(store_path)?;
    let queue = Arc::new(queue::IngressQueue::new(queue_capacity)?);
    let poller = poller::EventPoller::new(
        auth,
        Arc::new(std::sync::Mutex::new(store)),
        Arc::clone(&queue),
    )?;
    let source = KickoffIngressSource::new(Arc::clone(&queue));
    Ok((queue, poller, source))
}
