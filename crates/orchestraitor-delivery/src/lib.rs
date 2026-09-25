//! Spec-driven autonomous delivery (spec §9.33).
//!
//! This crate owns the orchestration machinery that turns the project backlog
//! into merge-ready change sets: task metadata (§9.33.2), the backlog DAG,
//! the runner loop, failure classification, and the adversarial review loop.
//! Security decisions are never made here — policy, approvals, sandboxing, and
//! receipts belong to Arbitraitor (spec §2.2, §9.33.7).

pub mod convergence;
pub mod dag;
pub mod failures;
pub mod findings;
pub mod metadata;
pub mod review_loop;
pub mod schedule;

pub use convergence::{
    BlockedReason, ConvergenceError, ConvergenceInput, ConvergenceVerdict,
    DEFAULT_HARD_LOOP_CEILING,
};
pub use dag::{DagError, TaskDag};
pub use failures::{
    DEFAULT_RETRY_DELAY_MS, DeliveryPhase, FailureClass, FailureLedger, FailureRecord,
    RetryDecision, classify,
};
pub use findings::{
    FindingError, FindingId, FindingLedger, FindingStatus, LedgerEntry, RecordOutcome,
    ReviewFinding,
};
pub use metadata::{
    Autonomy, BacklogTaskId, CompletionEvidence, DomainId, MetadataError, RiskClass,
    SCHEMA_VERSION, SpecRef, TaskMetadata, VerificationRef,
};
pub use review_loop::{ReviewLoopConfig, ReviewLoopConfigError, Severity};
pub use schedule::{ParallelScheduler, SchedulerConfig, SchedulerConfigError};
