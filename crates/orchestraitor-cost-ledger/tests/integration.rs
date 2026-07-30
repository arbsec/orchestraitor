//! Integration tests for the cost ledger with a stub provider transport.
//!
//! Exercises spec §9.19.4-§9.19.7: a stub [`ProviderTransport`] returns
//! controlled token counts, which are recorded as per-call cost entries,
//! rolled up per domain, attributed to subscriptions with `measured` /
//! `estimated` / `user-configured` labels, and enforced through soft-cap
//! warnings and hard-cap fallback routing. API spend and subscription
//! utilization are kept in separate tables (spec §9.19.4).

#![allow(clippy::unwrap_used)]

use async_trait::async_trait;
use chrono::Utc;
use orchestraitor_cost_ledger::{
    BudgetEnforcer, BudgetRequest, BudgetScope, CapKind, CapMetric, CostEntry, CostLedger,
    MonetaryCostBasis, PreSpawnDecision, RoutingFallback, Subscription, SubscriptionId,
    SubscriptionUtilizationEntry, UtilizationLabel,
};
use orchestraitor_model::{AgentId, ModelId, ProviderId, RepositoryId, SessionId};
use orchestraitor_provider_api::{
    DiscoveredModel, MessageRole, ModelEvent, ModelEventStream, ModelMessage, ModelRequest,
    ProviderCapabilities, ProviderDescriptor, ProviderHealth, ProviderHealthStatus,
    ProviderProtocol, ProviderResult, ProviderTransport, TokenCount, TokenCountRequest,
};

// ── Stub provider transport ───────────────────────────────────────────

/// Stub provider that returns controlled token counts via `ModelEvent::Usage`.
///
/// This is a test-only transport: it never touches the network and always
/// emits the token counts it was constructed with.
struct StubProvider {
    descriptor: ProviderDescriptor,
    input_tokens: u64,
    output_tokens: u64,
    reasoning_tokens: u64,
    cached_tokens: u64,
}

impl StubProvider {
    /// Creates a stub provider with the given token counts.
    fn new(provider_id: &str, input: u64, output: u64, reasoning: u64, cached: u64) -> Self {
        Self {
            descriptor: ProviderDescriptor {
                id: ProviderId::from_string(provider_id.to_string()),
                display_name: format!("Stub {provider_id}"),
                protocol: ProviderProtocol::CustomPlugin,
                capabilities: ProviderCapabilities::default(),
            },
            input_tokens: input,
            output_tokens: output,
            reasoning_tokens: reasoning,
            cached_tokens: cached,
        }
    }

    /// Returns the token count this provider will emit.
    fn token_count(&self) -> TokenCount {
        TokenCount {
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            cached_tokens: self.cached_tokens,
            reasoning_tokens: self.reasoning_tokens,
        }
    }
}

#[async_trait]
impl ProviderTransport for StubProvider {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    async fn list_models(&self) -> ProviderResult<Vec<DiscoveredModel>> {
        Ok(Vec::new())
    }

    async fn stream(&self, _request: ModelRequest) -> ProviderResult<ModelEventStream> {
        let token_count = self.token_count();
        Ok(Box::new(
            vec![
                Ok(ModelEvent::Started),
                Ok(ModelEvent::Usage { token_count }),
                Ok(ModelEvent::Completed),
            ]
            .into_iter(),
        ))
    }

    async fn count_tokens(
        &self,
        _request: TokenCountRequest,
    ) -> ProviderResult<Option<TokenCount>> {
        Ok(Some(self.token_count()))
    }

    async fn health(&self) -> ProviderResult<ProviderHealth> {
        Ok(ProviderHealth {
            status: ProviderHealthStatus::Healthy,
            message: None,
        })
    }
}

// ── Test helpers ──────────────────────────────────────────────────────

/// Returns the canonical backend domain identifier used across tests.
fn backend_domain() -> AgentId {
    AgentId::from_string(String::from("backend"))
}

/// Returns the canonical Neuralwatt provider identifier.
fn neuralwatt_provider() -> ProviderId {
    ProviderId::from_string(String::from("neuralwatt"))
}

/// Returns the canonical GLM model identifier.
fn glm_model() -> ModelId {
    ModelId::from_string(String::from("glm-5.2"))
}

/// Builds a minimal model request for the stub provider.
fn model_request(provider: &ProviderId, model: &ModelId) -> ModelRequest {
    ModelRequest {
        provider_id: provider.clone(),
        model_id: model.clone(),
        messages: vec![ModelMessage {
            role: MessageRole::User,
            content: String::from("integration test prompt"),
        }],
        max_output_tokens: None,
        temperature: None,
        reasoning: None,
        structured_output: None,
        tool_choice: None,
        extensions: serde_json::Map::new(),
    }
}

/// Streams from the provider, collects the usage event, and records a
/// per-call cost entry in the ledger. Returns the recorded entry.
///
/// This helper bridges the provider transport and the cost ledger: it
/// extracts `TokenCount` from the `ModelEvent::Usage` event and maps it
/// to a `CostEntry` per spec §9.19.4.
async fn record_call(
    ledger: &CostLedger,
    provider: &dyn ProviderTransport,
    request_id: &str,
    domain: &AgentId,
    provider_id: &ProviderId,
    model_id: &ModelId,
) -> CostEntry {
    let stream = provider
        .stream(model_request(provider_id, model_id))
        .await
        .unwrap();
    let mut usage = None;
    for event in stream {
        if let ModelEvent::Usage { token_count } = event.unwrap() {
            usage = Some(token_count);
        }
    }
    let token_count = usage.unwrap();
    let now = Utc::now();
    let entry = CostEntry {
        model: model_id.clone(),
        provider: provider_id.clone(),
        agent_domain_id: domain.clone(),
        role: String::from("implementing"),
        project: String::from("project-a"),
        session: SessionId::from_string(String::from("sess-a")),
        repository: RepositoryId::from_string(String::from("repo-a")),
        input_tokens: token_count.input_tokens,
        output_tokens: token_count.output_tokens,
        reasoning_tokens: token_count.reasoning_tokens,
        cache_read_tokens: token_count.cached_tokens,
        cache_write_tokens: 0,
        request_count: 1,
        request_id: String::from(request_id),
        parent_request_id: None,
        started_at: now,
        completed_at: now,
        wall_ms: 42,
        monetary_cost_measured: Some(0.001),
        monetary_cost_estimated: None,
        monetary_cost_basis: MonetaryCostBasis::ProviderMeasured,
        subscription_attribution_id: None,
        routing_decision: String::from("domain-default"),
    };
    ledger.api_spend().insert_cost_entry(&entry).unwrap();
    entry
}

/// Builds a subscription with the given soft and hard token caps.
fn subscription_with_caps(
    id: &str,
    provider: &ProviderId,
    soft_cap: Option<u64>,
    hard_cap: Option<u64>,
) -> Subscription {
    Subscription {
        id: SubscriptionId::from_string(String::from(id)),
        provider: provider.clone(),
        billing_period: String::from("monthly"),
        monthly_price_usd: Some(49.0),
        included_tokens: Some(50_000_000),
        soft_cap_tokens: soft_cap,
        hard_cap_tokens: hard_cap,
        active_time_cap_minutes_per_day: None,
        reset_at: String::from("monthly:1"),
    }
}

// ── Per-call entry (spec §9.19.4) ─────────────────────────────────────

/// Provider usage is recorded as a per-call cost entry with correct token
/// counts, provider, and model attribution.
#[tokio::test]
async fn provider_usage_recorded_as_per_call_entry() {
    let ledger = CostLedger::open_in_memory().unwrap();
    let provider = StubProvider::new("neuralwatt", 100, 50, 25, 0);
    let domain = backend_domain();
    let pid = neuralwatt_provider();
    let mid = glm_model();

    let entry = record_call(&ledger, &provider, "req-1", &domain, &pid, &mid).await;

    assert_eq!(entry.input_tokens, 100);
    assert_eq!(entry.output_tokens, 50);
    assert_eq!(entry.reasoning_tokens, 25);
    assert_eq!(entry.cache_read_tokens, 0);
    assert_eq!(entry.total_tokens(), 175);
    assert_eq!(entry.provider, pid);
    assert_eq!(entry.model, mid);
    assert_eq!(entry.agent_domain_id, domain);
}

// ── Per-domain rollup (spec §9.19.4) ──────────────────────────────────

/// Multiple provider calls to the same domain aggregate correctly in the
/// per-domain rollup.
#[tokio::test]
async fn per_domain_rollup_aggregates_multiple_provider_calls() {
    let ledger = CostLedger::open_in_memory().unwrap();
    let provider = StubProvider::new("neuralwatt", 100, 50, 25, 0);
    let domain = backend_domain();
    let pid = neuralwatt_provider();
    let mid = glm_model();

    record_call(&ledger, &provider, "req-1", &domain, &pid, &mid).await;
    record_call(&ledger, &provider, "req-2", &domain, &pid, &mid).await;
    record_call(&ledger, &provider, "req-3", &domain, &pid, &mid).await;

    let rollup = ledger.api_spend().domain_rollup(&domain).unwrap().unwrap();

    assert_eq!(rollup.agent_domain_id, domain);
    assert_eq!(rollup.input_tokens, 300);
    assert_eq!(rollup.output_tokens, 150);
    assert_eq!(rollup.reasoning_tokens, 75);
    assert_eq!(rollup.total_tokens(), 525);
    assert_eq!(rollup.request_count, 3);
}

// ── Subscription utilization labels (spec §9.19.4) ───────────────────

/// Subscription utilization with the `measured` label round-trips through
/// the ledger. A measured subscription without a user-supplied price has
/// no invented monetary cost.
#[tokio::test]
async fn subscription_utilization_measured_label_round_trips() {
    let ledger = CostLedger::open_in_memory().unwrap();
    let entry = SubscriptionUtilizationEntry {
        subscription_id: SubscriptionId::from_string(String::from("neuralwatt-monthly")),
        request_id: String::from("req-measured"),
        label: UtilizationLabel::Measured,
        consumed_tokens: 5_000,
        quota_tokens: Some(50_000_000),
        monthly_price_usd: None,
    };

    ledger
        .subscription_utilization()
        .insert_utilization(&entry)
        .unwrap();

    let label = ledger
        .subscription_utilization()
        .label_for_request("req-measured")
        .unwrap();

    assert_eq!(label, Some(UtilizationLabel::Measured));
    assert_eq!(entry.user_configured_cost_usd(), None);
}

/// Subscription utilization with the `estimated` label round-trips through
/// the ledger.
#[tokio::test]
async fn subscription_utilization_estimated_label_round_trips() {
    let ledger = CostLedger::open_in_memory().unwrap();
    let entry = SubscriptionUtilizationEntry {
        subscription_id: SubscriptionId::from_string(String::from("zai-monthly")),
        request_id: String::from("req-estimated"),
        label: UtilizationLabel::Estimated,
        consumed_tokens: 12_000,
        quota_tokens: Some(100_000_000),
        monthly_price_usd: None,
    };

    ledger
        .subscription_utilization()
        .insert_utilization(&entry)
        .unwrap();

    let label = ledger
        .subscription_utilization()
        .label_for_request("req-estimated")
        .unwrap();

    assert_eq!(label, Some(UtilizationLabel::Estimated));
}

/// Subscription utilization with the `user-configured` label round-trips
/// and derives USD only when the user supplied a monthly price.
#[tokio::test]
async fn subscription_utilization_user_configured_label_round_trips() {
    let ledger = CostLedger::open_in_memory().unwrap();
    let entry = SubscriptionUtilizationEntry {
        subscription_id: SubscriptionId::from_string(String::from("copilot-monthly")),
        request_id: String::from("req-user-configured"),
        label: UtilizationLabel::UserConfigured,
        consumed_tokens: 3_000,
        quota_tokens: Some(30_000),
        monthly_price_usd: Some(20.0),
    };

    ledger
        .subscription_utilization()
        .insert_utilization(&entry)
        .unwrap();

    let label = ledger
        .subscription_utilization()
        .label_for_request("req-user-configured")
        .unwrap();

    assert_eq!(label, Some(UtilizationLabel::UserConfigured));
    assert!(
        (entry.user_configured_cost_usd().unwrap() - 2.0).abs() < f64::EPSILON,
        "user-configured cost should be 20.0 * (3000 / 30000) = 2.0"
    );
}

/// A flat-rate subscription without enough pricing data has no invented
/// monetary cost (spec §9.19.4 acceptance criterion).
#[tokio::test]
async fn flat_rate_subscription_without_price_has_no_invented_cost() {
    let entry = SubscriptionUtilizationEntry {
        subscription_id: SubscriptionId::from_string(String::from("flat-rate")),
        request_id: String::from("req-no-price"),
        label: UtilizationLabel::Measured,
        consumed_tokens: 50,
        quota_tokens: Some(100),
        monthly_price_usd: None,
    };

    assert_eq!(entry.user_configured_cost_usd(), None);
}

/// A cost entry attributed to a flat-rate subscription with
/// `UtilizationOnly` basis has no invented monetary cost in the rollup.
#[tokio::test]
async fn flat_rate_cost_entry_rollup_has_no_invented_monetary_cost() {
    let ledger = CostLedger::open_in_memory().unwrap();
    let provider = StubProvider::new("neuralwatt", 100, 50, 0, 0);
    let domain = backend_domain();
    let pid = neuralwatt_provider();
    let mid = glm_model();

    // Stream from the provider to get controlled token counts.
    let stream = provider.stream(model_request(&pid, &mid)).await.unwrap();
    let mut usage = None;
    for event in stream {
        if let ModelEvent::Usage { token_count } = event.unwrap() {
            usage = Some(token_count);
        }
    }
    let token_count = usage.unwrap();

    // Record with UtilizationOnly basis — no monetary cost invented.
    let now = Utc::now();
    let entry = CostEntry {
        model: mid,
        provider: pid,
        agent_domain_id: domain.clone(),
        role: String::from("implementing"),
        project: String::from("project-a"),
        session: SessionId::from_string(String::from("sess-a")),
        repository: RepositoryId::from_string(String::from("repo-a")),
        input_tokens: token_count.input_tokens,
        output_tokens: token_count.output_tokens,
        reasoning_tokens: token_count.reasoning_tokens,
        cache_read_tokens: token_count.cached_tokens,
        cache_write_tokens: 0,
        request_count: 1,
        request_id: String::from("req-flat-rate"),
        parent_request_id: None,
        started_at: now,
        completed_at: now,
        wall_ms: 42,
        monetary_cost_measured: None,
        monetary_cost_estimated: None,
        monetary_cost_basis: MonetaryCostBasis::UtilizationOnly,
        subscription_attribution_id: Some(SubscriptionId::from_string(String::from("flat-rate"))),
        routing_decision: String::from("domain-default"),
    };
    ledger.api_spend().insert_cost_entry(&entry).unwrap();

    let rollup = ledger.api_spend().domain_rollup(&domain).unwrap().unwrap();

    assert!(rollup.monetary_cost_measured.abs() < f64::EPSILON);
    assert!(rollup.monetary_cost_estimated.abs() < f64::EPSILON);
}

// ── Separate tables (spec §9.19.4) ────────────────────────────────────

/// API spend and subscription utilization are stored in separate tables;
/// subscription utilization does not leak into the API spend rollup.
#[tokio::test]
async fn api_spend_and_subscription_utilization_tables_are_separate() {
    let ledger = CostLedger::open_in_memory().unwrap();
    let provider = StubProvider::new("neuralwatt", 100, 50, 0, 0);
    let domain = backend_domain();
    let pid = neuralwatt_provider();
    let mid = glm_model();

    // Record API spend from the provider.
    record_call(&ledger, &provider, "req-spend", &domain, &pid, &mid).await;

    // Record subscription utilization in the separate table.
    let util = SubscriptionUtilizationEntry {
        subscription_id: SubscriptionId::from_string(String::from("neuralwatt-monthly")),
        request_id: String::from("req-spend"),
        label: UtilizationLabel::Measured,
        consumed_tokens: 150,
        quota_tokens: Some(50_000_000),
        monthly_price_usd: None,
    };
    ledger
        .subscription_utilization()
        .insert_utilization(&util)
        .unwrap();

    // Both tables exist.
    assert!(ledger.has_table("cost_entries").unwrap());
    assert!(ledger.has_table("subscription_utilization").unwrap());

    // The subscription utilization (150 tokens) does not appear in the
    // API spend rollup (which only has the 150-token cost entry).
    let rollup = ledger.api_spend().domain_rollup(&domain).unwrap().unwrap();
    assert_eq!(rollup.total_tokens(), 150);
}

// ── Soft-cap warning (spec §9.19.5) ──────────────────────────────────

/// Crossing `soft_cap_tokens` emits a soft-cap warning event but does
/// not block spawn.
#[tokio::test]
async fn soft_cap_warning_when_crossing_soft_cap_tokens() {
    let ledger = CostLedger::open_in_memory().unwrap();
    let pid = neuralwatt_provider();
    let domain = backend_domain();

    // Configure subscription metadata with soft_cap_tokens = 1000.
    let sub = subscription_with_caps("neuralwatt-monthly", &pid, Some(1000), None);
    ledger.insert_subscription(&sub).unwrap();

    // Set up domain budget cap matching the subscription's soft_cap.
    let budget_id = ledger
        .insert_budget(BudgetScope::Domain, "backend")
        .unwrap();
    ledger
        .insert_cap(budget_id, CapMetric::Tokens, CapKind::Soft, 1000.0)
        .unwrap();

    // Record usage below the soft cap (600 tokens).
    let provider = StubProvider::new("neuralwatt", 400, 200, 0, 0);
    let mid = glm_model();
    record_call(&ledger, &provider, "req-below", &domain, &pid, &mid).await;

    // Next call estimate pushes total past the soft cap (600 + 500 = 1100 > 1000).
    let request = BudgetRequest {
        scope: BudgetScope::Domain,
        scope_id: String::from("backend"),
        token_estimate: 500,
        cost_estimate: None,
        fallback_route: None,
    };
    let decision = BudgetEnforcer::new(&ledger)
        .pre_spawn_check(&request)
        .unwrap();

    assert!(
        matches!(&decision, PreSpawnDecision::SoftWarn(event) if event.cap_kind == CapKind::Soft),
        "expected SoftWarn when crossing soft_cap_tokens, got {decision:?}"
    );
    assert!(!decision.blocks_spawn());
}

// ── Hard-cap block (spec §9.19.5) ────────────────────────────────────

/// Crossing `hard_cap_tokens` blocks spawn and surfaces the configured
/// fallback route to an alternate provider.
#[tokio::test]
async fn hard_cap_blocks_and_routes_to_alternate_provider_when_configured() {
    let ledger = CostLedger::open_in_memory().unwrap();
    let pid = neuralwatt_provider();
    let domain = backend_domain();

    // Configure subscription metadata with hard_cap_tokens = 5000.
    let sub = subscription_with_caps("neuralwatt-monthly", &pid, None, Some(5000));
    ledger.insert_subscription(&sub).unwrap();

    // Set up domain budget cap matching the subscription's hard_cap.
    let budget_id = ledger
        .insert_budget(BudgetScope::Domain, "backend")
        .unwrap();
    ledger
        .insert_cap(budget_id, CapMetric::Tokens, CapKind::Hard, 5000.0)
        .unwrap();

    // Record usage below the hard cap (3000 tokens).
    let provider = StubProvider::new("neuralwatt", 2000, 1000, 0, 0);
    let mid = glm_model();
    record_call(&ledger, &provider, "req-below", &domain, &pid, &mid).await;

    // Fallback route to an alternate provider.
    let fallback = RoutingFallback {
        provider: String::from("openai"),
        model: String::from("gpt-4"),
        reason: String::from("neuralwatt hard_cap_tokens crossed"),
    };

    // Next call estimate pushes total past the hard cap (3000 + 2500 = 5500 > 5000).
    let request = BudgetRequest {
        scope: BudgetScope::Domain,
        scope_id: String::from("backend"),
        token_estimate: 2500,
        cost_estimate: None,
        fallback_route: Some(fallback.clone()),
    };
    let decision = BudgetEnforcer::new(&ledger)
        .pre_spawn_check(&request)
        .unwrap();

    assert!(decision.blocks_spawn());
    assert!(
        matches!(&decision, PreSpawnDecision::HardCap { fallback_route: Some(route), .. } if route == &fallback),
        "expected HardCap with fallback route to alternate provider, got {decision:?}"
    );
}

/// Crossing `hard_cap_tokens` without a configured fallback still blocks
/// spawn but surfaces `None` for the fallback route.
#[tokio::test]
async fn hard_cap_blocks_without_fallback_when_not_configured() {
    let ledger = CostLedger::open_in_memory().unwrap();
    let pid = neuralwatt_provider();
    let domain = backend_domain();

    // Configure subscription metadata with hard_cap_tokens = 5000.
    let sub = subscription_with_caps("neuralwatt-monthly", &pid, None, Some(5000));
    ledger.insert_subscription(&sub).unwrap();

    // Set up domain budget cap matching the subscription's hard_cap.
    let budget_id = ledger
        .insert_budget(BudgetScope::Domain, "backend")
        .unwrap();
    ledger
        .insert_cap(budget_id, CapMetric::Tokens, CapKind::Hard, 5000.0)
        .unwrap();

    // Record usage below the hard cap (3000 tokens).
    let provider = StubProvider::new("neuralwatt", 2000, 1000, 0, 0);
    let mid = glm_model();
    record_call(&ledger, &provider, "req-below", &domain, &pid, &mid).await;

    // No fallback route configured.
    let request = BudgetRequest {
        scope: BudgetScope::Domain,
        scope_id: String::from("backend"),
        token_estimate: 2500,
        cost_estimate: None,
        fallback_route: None,
    };
    let decision = BudgetEnforcer::new(&ledger)
        .pre_spawn_check(&request)
        .unwrap();

    assert!(decision.blocks_spawn());
    assert!(
        matches!(
            &decision,
            PreSpawnDecision::HardCap {
                fallback_route: None,
                ..
            }
        ),
        "expected HardCap with no fallback route, got {decision:?}"
    );
}
