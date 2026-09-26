//! Spec-driven autonomous delivery (spec §9.33).
//!
//! This crate owns the orchestration machinery that turns the project backlog
//! into merge-ready change sets: task metadata (§9.33.2), the backlog DAG,
//! the runner loop, failure classification, and the adversarial review loop.
//! Security decisions are never made here — policy, approvals, sandboxing, and
//! receipts belong to Arbitraitor (spec §2.2, §9.33.7).

pub mod convergence;
pub mod dag;
pub mod escalation;
pub mod failures;
pub mod findings;
pub mod metadata;
pub mod retry_rules;
pub mod review_loop;
pub mod reviewer_selection;
pub mod runner;
pub mod schedule;

pub use convergence::{
    BlockedReason, ConvergenceError, ConvergenceInput, ConvergenceVerdict,
    DEFAULT_HARD_LOOP_CEILING,
};
pub use dag::{DagError, TaskDag};
pub use escalation::{
    EscalationOutcome, EscalationPolicy, EscalationPolicyError, EscalationState, EscalationStep,
    next_escalation,
};
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
pub use retry_rules::{
    DEFAULT_BASE_DELAY_MS, DEFAULT_MAX_DELAY_MS, IdempotencyProof, RetryGate, RetrySchedule,
    RetryScheduleError,
};
pub use review_loop::{ReviewLoopConfig, ReviewLoopConfigError, Severity};
pub use reviewer_selection::{
    ChangeSetProfile, GENERAL_DOMAIN, Language, ROLE_REVIEWING, ReviewerSelectionError,
    ReviewerSet, ReviewerSlot, SECURITY_DOMAIN, SelectionReason, TESTING_DOMAIN, select_reviewers,
};
pub use runner::{
    AttemptOutcome, BacklogRunner, BlockReason, RunnerError, RunnerEvent, RunnerInput, StopReason,
};
pub use schedule::{ParallelScheduler, SchedulerConfig, SchedulerConfigError};
