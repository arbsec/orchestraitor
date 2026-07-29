//! Integration tests for the cost ledger public API.

#![allow(clippy::unwrap_used)]

use chrono::Utc;
use orchestraitor_cost_ledger::{
    BudgetEnforcer, BudgetRequest, BudgetScope, CapKind, CapMetric, CostEntry, CostLedger,
    MonetaryCostBasis, PreSpawnDecision, RoutingFallback, SubscriptionId,
    SubscriptionUtilizationEntry, UtilizationLabel,
};
use orchestraitor_model::{AgentId, ModelId, ProviderId, RepositoryId, SessionId};

#[test]
fn insert_cost_entry_queries_per_domain_rollup() {
    let ledger = CostLedger::open_in_memory().unwrap();
    let domain = AgentId::from_string(String::from("backend"));
    let entry = sample_cost_entry(domain.clone(), "req-rollup", 100, 50);

    ledger.api_spend().insert_cost_entry(&entry).unwrap();
    let rollup = ledger.api_spend().domain_rollup(&domain).unwrap().unwrap();

    assert_eq!(rollup.agent_domain_id, domain);
    assert_eq!(rollup.input_tokens, 100);
    assert_eq!(rollup.output_tokens, 50);
    assert_eq!(rollup.total_tokens(), 175);
    assert_eq!(rollup.request_count, 1);
    assert!((rollup.monetary_cost_measured - 0.25).abs() < f64::EPSILON);
}

#[test]
fn hard_cap_blocks_spawn_and_routes_to_fallback() {
    let ledger = CostLedger::open_in_memory().unwrap();
    let entry = sample_cost_entry(
        AgentId::from_string(String::from("backend")),
        "req-cap",
        90,
        0,
    );
    ledger.api_spend().insert_cost_entry(&entry).unwrap();
    let budget_id = ledger
        .insert_budget(BudgetScope::Project, "project-a")
        .unwrap();
    ledger
        .insert_cap(budget_id, CapMetric::Tokens, CapKind::Hard, 100.0)
        .unwrap();
    let fallback = RoutingFallback {
        provider: String::from("fallback-provider"),
        model: String::from("fallback-model"),
        reason: String::from("project token hard cap crossed"),
    };
    let request = BudgetRequest {
        scope: BudgetScope::Project,
        scope_id: String::from("project-a"),
        token_estimate: 11,
        cost_estimate: None,
        fallback_route: Some(fallback.clone()),
    };

    let decision = BudgetEnforcer::new(&ledger)
        .pre_spawn_check(&request)
        .unwrap();

    assert!(decision.blocks_spawn());
    assert!(
        matches!(decision, PreSpawnDecision::HardCap { fallback_route: Some(route), .. } if route == fallback)
    );
}

#[test]
fn soft_cap_returns_warning_event() {
    let ledger = CostLedger::open_in_memory().unwrap();
    let budget_id = ledger
        .insert_budget(BudgetScope::Project, "project-a")
        .unwrap();
    ledger
        .insert_cap(budget_id, CapMetric::Tokens, CapKind::Soft, 10.0)
        .unwrap();
    let request = BudgetRequest {
        scope: BudgetScope::Project,
        scope_id: String::from("project-a"),
        token_estimate: 11,
        cost_estimate: None,
        fallback_route: None,
    };

    let decision = BudgetEnforcer::new(&ledger)
        .pre_spawn_check(&request)
        .unwrap();

    assert!(
        matches!(&decision, PreSpawnDecision::SoftWarn(event) if event.cap_kind == CapKind::Soft)
    );
    assert!(!decision.blocks_spawn());
}

#[test]
fn subscription_utilization_carries_correct_label() {
    let ledger = CostLedger::open_in_memory().unwrap();
    let entry = SubscriptionUtilizationEntry {
        subscription_id: SubscriptionId::from_string(String::from("neuralwatt-monthly")),
        request_id: String::from("req-sub"),
        label: UtilizationLabel::UserConfigured,
        consumed_tokens: 50,
        quota_tokens: Some(100),
        monthly_price_usd: Some(20.0),
    };

    ledger
        .subscription_utilization()
        .insert_utilization(&entry)
        .unwrap();
    let label = ledger
        .subscription_utilization()
        .label_for_request("req-sub")
        .unwrap();

    assert_eq!(label, Some(UtilizationLabel::UserConfigured));
    assert_eq!(entry.user_configured_cost_usd(), Some(10.0));
}

#[test]
fn subscription_without_user_price_has_no_invented_cost() {
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

#[test]
fn api_spend_and_subscription_utilization_tables_are_separate() {
    let ledger = CostLedger::open_in_memory().unwrap();
    let domain = AgentId::from_string(String::from("backend"));
    let utilization = SubscriptionUtilizationEntry {
        subscription_id: SubscriptionId::from_string(String::from("sub")),
        request_id: String::from("req-util-only"),
        label: UtilizationLabel::Measured,
        consumed_tokens: 1,
        quota_tokens: None,
        monthly_price_usd: None,
    };

    ledger
        .subscription_utilization()
        .insert_utilization(&utilization)
        .unwrap();

    assert!(ledger.has_table("cost_entries").unwrap());
    assert!(ledger.has_table("subscription_utilization").unwrap());
    assert_eq!(ledger.api_spend().domain_rollup(&domain).unwrap(), None);
}

fn sample_cost_entry(
    domain: AgentId,
    request_id: &str,
    input_tokens: u64,
    output_tokens: u64,
) -> CostEntry {
    let now = Utc::now();
    CostEntry {
        model: ModelId::from_string(String::from("glm-5.2")),
        provider: ProviderId::from_string(String::from("neuralwatt")),
        agent_domain_id: domain,
        role: String::from("implementing"),
        project: String::from("project-a"),
        session: SessionId::from_string(String::from("sess-a")),
        repository: RepositoryId::from_string(String::from("repo-a")),
        input_tokens,
        output_tokens,
        reasoning_tokens: 25,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        request_count: 1,
        request_id: String::from(request_id),
        parent_request_id: None,
        started_at: now,
        completed_at: now,
        wall_ms: 25,
        monetary_cost_measured: Some(0.25),
        monetary_cost_estimated: None,
        monetary_cost_basis: MonetaryCostBasis::ProviderMeasured,
        subscription_attribution_id: None,
        routing_decision: String::from("project-default"),
    }
}
