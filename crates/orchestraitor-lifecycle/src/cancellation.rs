//! Cancellation propagation and cleanup accounting.

use std::future::Future;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use tokio::time::timeout;

/// Identifier for a process, handle, socket, child, or sandbox resource being cleaned up.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ResourceId(String);

impl ResourceId {
    /// Creates a resource identifier from a non-secret label.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Returns the identifier string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Cancellation token backed by a bounded watch channel.
#[derive(Debug, Clone)]
pub struct CancellationToken {
    receiver: watch::Receiver<bool>,
}

impl CancellationToken {
    /// Returns true after cancellation has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        *self.receiver.borrow()
    }

    /// Waits until cancellation is requested.
    pub async fn cancelled(&mut self) {
        while !*self.receiver.borrow_and_update() {
            if self.receiver.changed().await.is_err() {
                return;
            }
        }
    }
}

/// Report returned by process/resource cleanup code.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CleanupReport {
    /// Resources released inside the grace period.
    pub released: Vec<ResourceId>,
    /// Resources that could not be released and must be recorded in audit output.
    pub unreleased: Vec<ResourceId>,
}

/// Final cancellation propagation outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancellationReport {
    /// Whether cleanup finished inside the bounded grace period.
    pub completed_within_grace: bool,
    /// Cleanup accounting, including unreleased resources.
    pub cleanup: CleanupReport,
}

/// Cancellation classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CancellationOutcome {
    /// Cancellation and cleanup completed inside the grace period.
    Released,
    /// Grace expired; controller should reap worker/sandbox and audit leftovers.
    GraceExpired,
}

/// Propagates cancellation tokens and bounds cleanup by a grace period.
#[derive(Debug, Clone)]
pub struct CancellationController {
    sender: watch::Sender<bool>,
    grace_period: Duration,
}

impl CancellationController {
    /// Creates a cancellation controller with a bounded grace period.
    #[must_use]
    pub fn new(grace_period: Duration) -> Self {
        let (sender, _receiver) = watch::channel(false);
        Self {
            sender,
            grace_period,
        }
    }

    /// Returns a child token workers can observe.
    #[must_use]
    pub fn token(&self) -> CancellationToken {
        CancellationToken {
            receiver: self.sender.subscribe(),
        }
    }

    /// Requests cancellation, then waits for cleanup until the configured grace period expires.
    pub async fn cancel_with_cleanup<F>(
        &self,
        tracked_resources: Vec<ResourceId>,
        cleanup: F,
    ) -> (CancellationOutcome, CancellationReport)
    where
        F: Future<Output = CleanupReport>,
    {
        let _receiver_count = self.sender.send_replace(true);
        match timeout(self.grace_period, cleanup).await {
            Ok(report) => (
                CancellationOutcome::Released,
                CancellationReport {
                    completed_within_grace: true,
                    cleanup: report,
                },
            ),
            Err(_elapsed) => (
                CancellationOutcome::GraceExpired,
                CancellationReport {
                    completed_within_grace: false,
                    cleanup: CleanupReport {
                        released: Vec::new(),
                        unreleased: tracked_resources,
                    },
                },
            ),
        }
    }
}
