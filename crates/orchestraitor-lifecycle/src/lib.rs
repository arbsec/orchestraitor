//! Task and session lifecycle state machine (spec §9.24, §9.26, §9.27.2-3).
//!
//! This crate validates orchestration lifecycle transitions, records replay-safe
//! idempotency metadata, models crash recovery and cancellation bookkeeping, and
//! persists partial results into the append-only event store. It contains no
//! sandboxing, authorization, or security-enforcement decision logic.

#![forbid(unsafe_code)]
#![allow(
    clippy::module_name_repetitions,
    reason = "public lifecycle types are clearer with lifecycle/checkpoint/lease prefixes"
)]

mod cancellation;
mod checkpoint;
mod error;
mod event_preservation;
mod lease;
mod recovery;
mod state_machine;

pub use cancellation::{
    CancellationController, CancellationOutcome, CancellationReport, CancellationToken,
    CleanupReport, ResourceId,
};
pub use checkpoint::{
    Checkpoint, CheckpointCursor, CheckpointPolicy, CheckpointTrigger, ReplayPlan,
};
pub use error::LifecycleError;
pub use event_preservation::{EventCursor, PartialResult, PartialResultKind, PartialResultStore};
pub use lease::{Lease, LeaseTtl};
pub use orchestraitor_model::SessionState;
pub use recovery::{LifecycleConfig, RecoveryAction, RecoveryDecision};
pub use state_machine::{
    IdempotencyKey, LifecycleRecord, StateMachine, TransitionAttempt, TransitionRecord,
};
