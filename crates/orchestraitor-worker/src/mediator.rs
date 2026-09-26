//! Bash mediation seam: every bash call crosses the Arbitraitor boundary.
//!
//! The production implementation ([`MediatedBashMediator`]) wraps
//! `MediatedWorker::spawn` + `MediatedWorker::run_bash` from
//! `orchestraitor-arbitraitor-client`'s mediation module (issue #311): the
//! preflight runs before the first execution surface exists, and the worker
//! crate implements no security primitive itself (spec §2.2).

use std::sync::Mutex;

use async_trait::async_trait;
use orchestraitor_arbitraitor_client::ArbitraitorClient;
use orchestraitor_arbitraitor_client::mediation::{MediatedWorker, WORKER_PLATFORM};

pub use orchestraitor_arbitraitor_client::mediation::{MediatedRun, MediationError};

/// Mediates bash execution through the Arbitraitor boundary.
///
/// The seam exists so tests can drive the loop deterministically; the only
/// production implementation is [`MediatedBashMediator`], which crosses the
/// #311 mediation module on every call.
#[async_trait]
pub trait BashMediator: Send + Sync {
    /// Runs a bash script through the mediated boundary.
    ///
    /// # Errors
    ///
    /// Returns [`MediationError`] when the boundary refuses or fails — always
    /// a typed, fail-closed refusal (spec §6.7).
    async fn run_bash(&self, script: &str) -> Result<MediatedRun, MediationError>;
}

/// Production [`BashMediator`]: the #311 mediated worker, spawned lazily on
/// the first bash call.
///
/// Lazy spawn keeps the fail-closed preflight contract exactly where it
/// belongs — before any execution surface exists — while letting a run whose
/// model never requests bash complete without probing the sandbox (the
/// preflight is host-dependent by design: it refuses on hosts without the
/// required controls, per ADR-0024 and the #755 stopgap).
pub struct MediatedBashMediator {
    client: ArbitraitorClient,
    worker: Mutex<Option<MediatedWorker>>,
}

impl MediatedBashMediator {
    /// Creates a mediator that will preflight and spawn on first use.
    #[must_use]
    pub fn new() -> Self {
        Self {
            client: ArbitraitorClient::default(),
            worker: Mutex::new(None),
        }
    }

    /// Returns the gated worker, spawning (and preflighting) on first use.
    fn worker(&self) -> Result<MediatedWorker, MediationError> {
        let mut guard = self.worker.lock().map_err(|_| MediationError::Bash {
            reason: "mediator-lock",
        })?;
        if let Some(worker) = guard.as_ref() {
            return Ok(worker.clone());
        }
        let spawned = MediatedWorker::spawn(&self.client, WORKER_PLATFORM)?;
        *guard = Some(spawned.clone());
        Ok(spawned)
    }
}

impl Default for MediatedBashMediator {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl BashMediator for MediatedBashMediator {
    async fn run_bash(&self, script: &str) -> Result<MediatedRun, MediationError> {
        let worker = self.worker()?;
        let script = script.to_string();
        tokio::task::spawn_blocking(move || worker.run_bash(script.as_bytes()))
            .await
            .map_err(|_| MediationError::Bash {
                reason: "task-join",
            })?
    }
}
