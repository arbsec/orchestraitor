//! Delivery seam for completed worker runs.
//!
//! The worker loop hands a completed task to a [`DeliverySink`]; the thin
//! slice ships [`PendingDeliverySink`], which reports every delivery as
//! pending. The real delivery path (worktree/commit/push/PR) is a separate
//! lane and wires in behind this trait — the loop never performs delivery
//! itself.

use async_trait::async_trait;
use serde::Serialize;
use thiserror::Error;

/// Input handed to the delivery seam for one completed task.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeliveryRequest {
    /// Completed task id.
    pub task_id: String,
    /// Model-authored completion summary.
    pub summary: String,
    /// Worktree-relative paths the worker wrote. All worker output begins
    /// untrusted (spec `40-arbitraitor-integration.md` §9.14); classification,
    /// scanning, and promotion authorization stay with Arbitraitor and the
    /// delivery lane.
    pub untrusted_writes: Vec<String>,
}

/// Delivery result reported back in the structured run result.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum DeliveryOutcome {
    /// A pull request was opened for the task.
    PrOpened {
        /// PR reference (URL or id) from the delivery path.
        reference: String,
    },
    /// Delivery is pending (the delivery path is not wired in this slice).
    Pending {
        /// Static reason code.
        reason: &'static str,
    },
}

/// Delivery failures carry static reason codes only (spec §9.23.4).
#[derive(Debug, Error)]
#[error("delivery failed: {reason}")]
pub struct DeliveryError {
    /// Static reason code.
    pub reason: &'static str,
}

/// Hand-off seam from a completed worker run to the delivery path.
#[async_trait]
pub trait DeliverySink: Send + Sync {
    /// Delivers one completed task.
    ///
    /// # Errors
    ///
    /// Returns [`DeliveryError`] when the delivery path rejects or fails; the
    /// loop converts this into a typed run failure.
    async fn deliver(&self, request: &DeliveryRequest) -> Result<DeliveryOutcome, DeliveryError>;
}

/// Bootstrap delivery sink: the delivery path is not wired in this slice, so
/// every completed task is reported pending (never silently dropped).
pub struct PendingDeliverySink;

#[async_trait]
impl DeliverySink for PendingDeliverySink {
    async fn deliver(&self, request: &DeliveryRequest) -> Result<DeliveryOutcome, DeliveryError> {
        let _ = request;
        Ok(DeliveryOutcome::Pending {
            reason: "delivery-path-not-wired",
        })
    }
}
