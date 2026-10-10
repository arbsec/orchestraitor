//! Adapter host supervisor with Arbitraitor-mediated start/send boundaries.
//!
//! The host owns adapter registration and audit-store multiplexing. It never
//! implements security primitives: start/send operations first call the
//! configured Arbitraitor client adapter and proceed only on Arbitraitor-owned
//! pass or warning verdicts.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::sync::Arc;

use orchestraitor_adapter_api::{
    AdapterError, AdapterEvent, AgentAdapter, AgentInput, AgentSession, StartRequest,
};
use orchestraitor_arbitraitor_client::{
    ArbitraitorClient, EvalContext, Finding, PolicyError, Verdict,
};
use orchestraitor_events::{
    AuditRecord, AuditStore, CURRENT_SCHEMA_VERSION, EventEnvelope, EventEnvelopeInput, EventError,
    EventQuery,
};
use orchestraitor_workspace::AdapterId;

#[cfg(test)]
mod tests;

/// Result type for adapter host operations.
pub type HostResult<T> = Result<T, AdapterHostError>;

/// Evaluates Arbitraitor policy before adapter side effects.
pub trait ArbitraitorPolicyEvaluator: Send + Sync {
    /// Evaluates one adapter operation with Arbitraitor-owned policy logic.
    ///
    /// # Errors
    /// Returns [`PolicyError`] when Arbitraitor cannot evaluate the policy.
    fn evaluate_adapter_operation(
        &self,
        policy_toml: &str,
        findings: &[Finding],
        context: &EvalContext,
    ) -> Result<Verdict, PolicyError>;
}

impl ArbitraitorPolicyEvaluator for ArbitraitorClient {
    fn evaluate_adapter_operation(
        &self,
        policy_toml: &str,
        findings: &[Finding],
        context: &EvalContext,
    ) -> Result<Verdict, PolicyError> {
        self.evaluate_policy(policy_toml, findings, context)
    }
}

/// Supervises registered adapters and multiplexes their events into one audit store.
pub struct AdapterSupervisor<S, A = ArbitraitorClient>
where
    S: AuditStore,
    A: ArbitraitorPolicyEvaluator,
{
    arbitraitor: A,
    audit_store: S,
    adapters: HashMap<AdapterId, Arc<dyn AgentAdapter>>,
}

impl<S, A> AdapterSupervisor<S, A>
where
    S: AuditStore,
    A: ArbitraitorPolicyEvaluator,
{
    /// Constructs an adapter supervisor over an audit store and Arbitraitor adapter.
    #[must_use]
    pub fn new(arbitraitor: A, audit_store: S) -> Self {
        Self {
            arbitraitor,
            audit_store,
            adapters: HashMap::new(),
        }
    }

    /// Registers an adapter implementation by manifest id.
    pub fn register_adapter(&mut self, adapter: Arc<dyn AgentAdapter>) {
        let adapter_id = adapter.manifest().id.clone();
        self.adapters.insert(adapter_id, adapter);
    }

    /// Returns a shared reference to the unified audit store.
    #[must_use]
    pub const fn audit_store(&self) -> &S {
        &self.audit_store
    }

    /// Starts a session after Arbitraitor evaluates the operation.
    ///
    /// # Errors
    /// Returns [`AdapterHostError`] when Arbitraitor does not produce a pass/warn
    /// verdict, the adapter is missing, or the adapter start fails.
    pub async fn start(&self, request: SupervisedStartRequest) -> HostResult<AgentSession> {
        self.evaluate_with_arbitraitor(&request.enforcement)?;
        let adapter = self.adapter_for(&request.adapter_id)?;
        adapter.start(request.request).await.map_err(Into::into)
    }

    /// Sends input to a session after Arbitraitor evaluates the operation.
    ///
    /// # Errors
    /// Returns [`AdapterHostError`] when Arbitraitor does not produce a pass/warn
    /// verdict, the adapter is missing, or the adapter send fails.
    pub async fn send(&self, request: SupervisedSendRequest) -> HostResult<()> {
        self.evaluate_with_arbitraitor(&request.enforcement)?;
        let adapter = self.adapter_for(&request.session.adapter_id)?;
        adapter
            .send(&request.session, request.input)
            .await
            .map_err(Into::into)
    }

    /// Drains one adapter event stream into the unified audit store.
    ///
    /// # Errors
    /// Returns [`AdapterHostError`] when the adapter is missing, its stream fails,
    /// or the audit store rejects an event.
    pub async fn multiplex_events(
        &mut self,
        session: &AgentSession,
    ) -> HostResult<Vec<AuditRecord>> {
        let adapter = self.adapter_for(&session.adapter_id)?;
        let stream = adapter.events(session).await?;
        let mut records = Vec::new();
        for event in stream {
            let record = self.append_adapter_event(event?)?;
            records.push(record);
        }
        Ok(records)
    }

    fn evaluate_with_arbitraitor(&self, request: &ArbitraitorEvaluationRequest) -> HostResult<()> {
        let verdict = self.arbitraitor.evaluate_adapter_operation(
            &request.policy_toml,
            &request.findings,
            &request.context,
        )?;
        match verdict {
            Verdict::Pass | Verdict::Warn => Ok(()),
            Verdict::Prompt | Verdict::Block | Verdict::Error | Verdict::Incomplete => {
                Err(AdapterHostError::ArbitraitorVerdict { verdict })
            }
        }
    }

    fn adapter_for(&self, adapter_id: &AdapterId) -> HostResult<Arc<dyn AgentAdapter>> {
        self.adapters
            .get(adapter_id)
            .cloned()
            .ok_or_else(|| AdapterHostError::AdapterMissing {
                adapter_id: adapter_id.clone(),
            })
    }

    fn append_adapter_event(&mut self, event: AdapterEvent) -> HostResult<AuditRecord> {
        let previous = self.last_record()?;
        let monotonic_seq = previous
            .as_ref()
            .map_or(1, |record| record.envelope.monotonic_seq.saturating_add(1));
        let prev_hash = previous.map(|record| record.hash);
        let envelope = EventEnvelope::try_new(EventEnvelopeInput {
            schema_version: CURRENT_SCHEMA_VERSION,
            monotonic_seq,
            wall_clock_ts: event.wall_clock_ts,
            correlation_id: event.correlation_id,
            parent_op_id: event.parent_op_id,
            category: event.category,
            payload: event.payload,
            prev_hash,
        })?;
        self.audit_store.append(envelope).map_err(Into::into)
    }

    fn last_record(&self) -> HostResult<Option<AuditRecord>> {
        let records = self.audit_store.query(&EventQuery {
            category: None,
            since_seq: None,
            until_seq: None,
            include_uninterpreted: true,
        })?;
        Ok(records.into_iter().last())
    }
}

/// Start request plus adapter id and Arbitraitor policy inputs.
pub struct SupervisedStartRequest {
    /// Adapter selected by the control plane.
    pub adapter_id: AdapterId,
    /// Adapter start request.
    pub request: StartRequest,
    /// Arbitraitor evaluation inputs for this operation.
    pub enforcement: ArbitraitorEvaluationRequest,
}

/// Send request plus Arbitraitor policy inputs.
pub struct SupervisedSendRequest {
    /// Running adapter session.
    pub session: AgentSession,
    /// Input to deliver if Arbitraitor permits the operation.
    pub input: AgentInput,
    /// Arbitraitor evaluation inputs for this operation.
    pub enforcement: ArbitraitorEvaluationRequest,
}

/// Arbitraitor policy inputs captured at the adapter host boundary.
pub struct ArbitraitorEvaluationRequest {
    /// Policy document evaluated by Arbitraitor.
    pub policy_toml: String,
    /// Arbitraitor findings available for this operation.
    pub findings: Vec<Finding>,
    /// Arbitraitor policy context for this operation.
    pub context: EvalContext,
}

/// Adapter host failures.
#[derive(Debug, thiserror::Error)]
pub enum AdapterHostError {
    /// Adapter id is not registered; startup fails closed.
    #[error("adapter `{adapter_id}` is not registered")]
    AdapterMissing {
        /// Missing adapter id.
        adapter_id: AdapterId,
    },
    /// Arbitraitor returned a verdict that does not permit immediate execution.
    #[error("Arbitraitor did not permit adapter operation: {verdict:?}")]
    ArbitraitorVerdict {
        /// Arbitraitor-owned verdict.
        verdict: Verdict,
    },
    /// Arbitraitor policy evaluation failed.
    #[error("Arbitraitor policy evaluation failed")]
    ArbitraitorPolicy(#[from] PolicyError),
    /// Adapter boundary failed.
    #[error(transparent)]
    Adapter(#[from] AdapterError),
    /// Audit-store write failed.
    #[error(transparent)]
    Audit(#[from] EventError),
}
