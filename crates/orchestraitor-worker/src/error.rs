//! Worker-crate error type.
//!
//! Every variant carries a static reason code: worker errors never embed
//! secrets, tool arguments, command lines, or captured tool output (spec
//! `40-arbitraitor-integration.md` §9.23.4 log-safety rule, mirroring the
//! mediation module's static-reason translation).

use thiserror::Error;

/// Errors from bootstrap-worker infrastructure (not per-attempt failures).
///
/// Per-attempt and per-run failures the loop can classify (budget exhaustion,
/// mediation refusal, provider errors, ...) are carried in
/// [`crate::WorkerRun`] as typed failures instead — `WorkerError` is reserved
/// for conditions that prevent a run from being produced at all.
#[derive(Debug, Error)]
pub enum WorkerError {
    /// The task worktree root cannot be used.
    #[error("worker worktree root is unusable: {reason}")]
    WorktreeRoot {
        /// Static reason code (e.g. `missing`, `not-a-directory`, `canonicalize-io`).
        reason: &'static str,
    },
    /// Building the bootstrap provider transport failed.
    #[error("bootstrap provider transport construction failed: {reason}")]
    Transport {
        /// Static reason code translated from the provider adapter error.
        reason: &'static str,
    },
    /// Serializing the structured worker result failed.
    #[error("worker result serialization failed: {reason}")]
    Serialize {
        /// Static reason code.
        reason: &'static str,
    },
}
