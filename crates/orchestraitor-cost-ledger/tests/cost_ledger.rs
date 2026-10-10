//! Integration tests for the cost ledger public API.

#![allow(clippy::unwrap_used, clippy::panic)]

use chrono::Utc;
use orchestraitor_cost_ledger::{
    BudgetEnforcer, BudgetRequest, BudgetScope, CapKind, CapMetric, CostEntry, CostLedger,
    EfficiencyGrouping, MonetaryCostBasis, PreSpawnDecision, RoutingFallback, SubscriptionId,
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
        profile: None,
    }
}

#[test]
fn project_budget_does_not_count_other_projects_cost() {
    // Regression guard for the MEDIUM finding where `scope_filter` mapped
    // Organization/User to a no-op (`?1 = ?1`) that counted every ledger row
    // against every org/user budget. After the fix, every scope must isolate
    // by its column; this test pins the contract for `Project`.
    let ledger = CostLedger::open_in_memory().unwrap();
    let decoy = cost_entry_with_project("project-decoy", "req-decoy", 10_000, 0);
    ledger.api_spend().insert_cost_entry(&decoy).unwrap();
    let real = cost_entry_with_project("project-a", "req-real", 100, 0);
    ledger.api_spend().insert_cost_entry(&real).unwrap();

    let budget_id = ledger
        .insert_budget(BudgetScope::Project, "project-a")
        .unwrap();
    ledger
        .insert_cap(budget_id, CapMetric::Tokens, CapKind::Hard, 500.0)
        .unwrap();
    let request = BudgetRequest {
        scope: BudgetScope::Project,
        scope_id: String::from("project-a"),
        token_estimate: 1,
        cost_estimate: None,
        fallback_route: None,
    };

    let decision = BudgetEnforcer::new(&ledger)
        .pre_spawn_check(&request)
        .unwrap();

    assert!(
        matches!(decision, PreSpawnDecision::Allow),
        "project-a budget must not count project-decoy's 10_000 tokens (got {decision:?})"
    );
}

#[test]
fn session_budget_does_not_count_other_sessions_cost() {
    // Regression guard for the same no-op `scope_filter` bug class, pinned for
    // the `Session` scope so future scope additions cannot silently leak.
    let ledger = CostLedger::open_in_memory().unwrap();
    let decoy = cost_entry_with_session("sess-decoy", "req-decoy", 10_000, 0);
    ledger.api_spend().insert_cost_entry(&decoy).unwrap();
    let real = cost_entry_with_session("sess-a", "req-real", 100, 0);
    ledger.api_spend().insert_cost_entry(&real).unwrap();

    let budget_id = ledger
        .insert_budget(BudgetScope::Session, "sess-a")
        .unwrap();
    ledger
        .insert_cap(budget_id, CapMetric::Tokens, CapKind::Hard, 500.0)
        .unwrap();
    let request = BudgetRequest {
        scope: BudgetScope::Session,
        scope_id: String::from("sess-a"),
        token_estimate: 1,
        cost_estimate: None,
        fallback_route: None,
    };

    let decision = BudgetEnforcer::new(&ledger)
        .pre_spawn_check(&request)
        .unwrap();

    assert!(
        matches!(decision, PreSpawnDecision::Allow),
        "sess-a budget must not count sess-decoy's 10_000 tokens (got {decision:?})"
    );
}

#[test]
fn domain_budget_does_not_count_other_domains_cost() {
    // Regression guard for the same no-op `scope_filter` bug class, pinned for
    // the `Domain` scope. `Agent` shares the same filter expression in
    // `scope_filter`, so this test transitively covers `Agent` at the SQL level.
    let ledger = CostLedger::open_in_memory().unwrap();
    let decoy = sample_cost_entry(
        AgentId::from_string(String::from("backend-decoy")),
        "req-decoy",
        10_000,
        0,
    );
    ledger.api_spend().insert_cost_entry(&decoy).unwrap();
    let real = sample_cost_entry(
        AgentId::from_string(String::from("backend")),
        "req-real",
        100,
        0,
    );
    ledger.api_spend().insert_cost_entry(&real).unwrap();

    let budget_id = ledger
        .insert_budget(BudgetScope::Domain, "backend")
        .unwrap();
    ledger
        .insert_cap(budget_id, CapMetric::Tokens, CapKind::Hard, 500.0)
        .unwrap();
    let request = BudgetRequest {
        scope: BudgetScope::Domain,
        scope_id: String::from("backend"),
        token_estimate: 1,
        cost_estimate: None,
        fallback_route: None,
    };

    let decision = BudgetEnforcer::new(&ledger)
        .pre_spawn_check(&request)
        .unwrap();

    assert!(
        matches!(decision, PreSpawnDecision::Allow),
        "backend budget must not count backend-decoy's 10_000 tokens (got {decision:?})"
    );
}

fn cost_entry_with_project(
    project: &str,
    request_id: &str,
    input_tokens: u64,
    output_tokens: u64,
) -> CostEntry {
    let mut entry = sample_cost_entry(
        AgentId::from_string(String::from("backend")),
        request_id,
        input_tokens,
        output_tokens,
    );
    entry.project = String::from(project);
    entry
}

fn cost_entry_with_session(
    session: &str,
    request_id: &str,
    input_tokens: u64,
    output_tokens: u64,
) -> CostEntry {
    let mut entry = sample_cost_entry(
        AgentId::from_string(String::from("backend")),
        request_id,
        input_tokens,
        output_tokens,
    );
    entry.session = SessionId::from_string(String::from(session));
    entry
}

/// Regression guard for issue #501: `all_agent_rollups` must group by the
/// domain-agent id across domains, summing token counters and request counts
/// per agent — one rollup row per agent, never one per entry.
#[test]
fn all_agent_rollups_group_by_agent_across_domains() {
    let ledger = CostLedger::open_in_memory().unwrap();

    // "backend" spans two entries in different sessions; "frontend" has one.
    let backend_one = sample_cost_entry(
        AgentId::from_string(String::from("backend")),
        "req-backend-1",
        100,
        50,
    );
    let backend_two = sample_cost_entry(
        AgentId::from_string(String::from("backend")),
        "req-backend-2",
        30,
        20,
    );
    let frontend = sample_cost_entry(
        AgentId::from_string(String::from("frontend")),
        "req-frontend-1",
        7,
        3,
    );
    for entry in [&backend_one, &backend_two, &frontend] {
        ledger.api_spend().insert_cost_entry(entry).unwrap();
    }

    let rollups = ledger.api_spend().all_agent_rollups().unwrap();

    assert_eq!(rollups.len(), 2, "one rollup per agent, not per entry");
    assert_eq!(rollups[0].agent_domain_id.as_str(), "backend");
    assert_eq!(rollups[0].input_tokens, 130);
    assert_eq!(rollups[0].output_tokens, 70);
    assert_eq!(rollups[0].reasoning_tokens, 50);
    assert_eq!(rollups[0].request_count, 2);
    assert_eq!(rollups[0].total_tokens(), 175 + 75);
    assert!(
        (rollups[0].monetary_cost_measured - 0.5).abs() < f64::EPSILON,
        "measured cost sums across the agent's entries"
    );
    assert_eq!(rollups[1].agent_domain_id.as_str(), "frontend");
    assert_eq!(rollups[1].input_tokens, 7);
    assert_eq!(rollups[1].output_tokens, 3);
    assert_eq!(rollups[1].request_count, 1);
}

/// Builds a receipt record with the given session and receipt counters.
fn receipt(
    session: &str,
    request_id: &str,
    candidate_tokens: u64,
    selected_tokens: u64,
    raw_tool_output_tokens: u64,
    compacted_tool_output_tokens: u64,
) -> orchestraitor_cost_ledger::ReceiptRecord {
    orchestraitor_cost_ledger::ReceiptRecord {
        request_id: String::from(request_id),
        session: SessionId::from_string(String::from(session)),
        task_class: String::from("backend"),
        budget_tokens: 100_000,
        candidate_tokens,
        selected_tokens,
        omitted_count: 1,
        raw_tool_output_tokens,
        compacted_tool_output_tokens,
        repeated_tokens_avoided: 0,
        prompt_cache_eligible_tokens: 0,
    }
}

/// §13.5.1: the savings ratio computes from summed receipt counters with
/// tool-output compaction included in both numerator and denominator.
#[test]
fn savings_ratio_uses_candidate_plus_raw_tool_baseline() {
    let ratio = orchestraitor_cost_ledger::TokenEfficiencyRollup::savings_from_receipt(
        200_000, // candidate
        80_000,  // selected
        20_000,  // raw tool output
        5_000,   // compacted tool output
    );
    let expected = 1.0 - (85_000_f64 / 220_000_f64);
    let got = ratio.unwrap_or_else(|| panic!("savings must be measured"));
    assert!(
        (got - expected).abs() < 1e-12,
        "ratio {got} must equal 1 - (selected+compacted)/(candidate+raw): {expected}"
    );
}

/// §13.5.1: zero candidate baseline carries no measurable savings — the
/// result is `None`, never a division by zero or a fabricated 0%.
#[test]
fn savings_ratio_is_none_for_zero_baseline() {
    assert_eq!(
        orchestraitor_cost_ledger::TokenEfficiencyRollup::savings_from_receipt(0, 0, 0, 0),
        None
    );
    assert_eq!(
        orchestraitor_cost_ledger::TokenEfficiencyRollup::savings_from_receipt(0, 10, 0, 0),
        None,
        "selected without a candidate baseline is not measurable savings"
    );
}

/// A session with cost entries but no receipts must report cost sums with
/// `None` savings — reporting 0% would fabricate a measurement.
#[test]
fn efficiency_without_receipts_reports_none_savings() {
    let ledger = CostLedger::open_in_memory().unwrap();
    ledger
        .api_spend()
        .insert_cost_entry(&cost_entry_with_session("sess-a", "req-1", 100, 50))
        .unwrap();

    let rollups = ledger
        .token_efficiency_rollups(EfficiencyGrouping::Session)
        .unwrap();

    assert_eq!(rollups.len(), 1);
    assert_eq!(rollups[0].session.as_deref(), Some("sess-a"));
    assert_eq!(rollups[0].input_tokens, 100);
    assert_eq!(rollups[0].output_tokens, 50);
    assert_eq!(rollups[0].candidate_tokens, None);
    assert_eq!(rollups[0].selected_tokens, None);
    assert_eq!(rollups[0].savings_ratio, None);
}

/// Per-session rollups join receipts to cost entries on the session id:
/// receipt deltas sum per session, savings derive from the sums.
#[test]
fn efficiency_joins_receipts_by_session() {
    let ledger = CostLedger::open_in_memory().unwrap();
    ledger
        .api_spend()
        .insert_cost_entry(&cost_entry_with_session("sess-a", "req-a1", 1_000, 10))
        .unwrap();
    ledger
        .api_spend()
        .insert_cost_entry(&cost_entry_with_session("sess-a", "req-a2", 2_000, 20))
        .unwrap();
    ledger
        .insert_context_receipt(&receipt("sess-a", "ctx-a1", 100_000, 60_000, 10_000, 4_000))
        .unwrap();
    ledger
        .insert_context_receipt(&receipt("sess-a", "ctx-a2", 50_000, 20_000, 10_000, 2_000))
        .unwrap();
    // A different session's receipt must not leak into sess-a's sums.
    ledger
        .insert_context_receipt(&receipt("sess-b", "ctx-b1", 999_999, 1, 0, 0))
        .unwrap();

    let rollups = ledger
        .token_efficiency_rollups(EfficiencyGrouping::Session)
        .unwrap();

    let sess_a = rollups
        .iter()
        .find(|rollup| rollup.session.as_deref() == Some("sess-a"))
        .unwrap_or_else(|| panic!("sess-a rollup must exist"));
    assert_eq!(sess_a.input_tokens, 3_000, "cost entries sum across calls");
    assert_eq!(
        sess_a.candidate_tokens,
        Some(150_000),
        "receipts sum per session"
    );
    assert_eq!(sess_a.selected_tokens, Some(80_000));
    assert_eq!(sess_a.raw_tool_output_tokens, Some(20_000));
    assert_eq!(sess_a.compacted_tool_output_tokens, Some(6_000));
    let ratio = sess_a
        .savings_ratio
        .unwrap_or_else(|| panic!("must be measured"));
    let expected = 1.0 - (86_000_f64 / 170_000_f64);
    assert!((ratio - expected).abs() < 1e-12);
}

/// Profile grouping rolls entries up by the profile label; entries without
/// a label group together as unprofiled (spec §13.5.1 A/B mechanics).
#[test]
fn efficiency_groups_by_profile_for_ab_comparison() {
    let ledger = CostLedger::open_in_memory().unwrap();
    let mut with_compaction = cost_entry_with_session("sess-a", "req-a", 1_000, 10);
    with_compaction.profile = Some(String::from("aggressive"));
    let mut without = cost_entry_with_session("sess-b", "req-b", 2_000, 20);
    without.profile = None;
    ledger
        .api_spend()
        .insert_cost_entry(&with_compaction)
        .unwrap();
    ledger.api_spend().insert_cost_entry(&without).unwrap();
    ledger
        .insert_context_receipt(&receipt("sess-a", "ctx-a", 100_000, 40_000, 0, 0))
        .unwrap();
    // sess-b has no receipt: its savings stay unmeasured even though its
    // cost tokens roll up under the same profile group.

    let rollups = ledger
        .token_efficiency_rollups(EfficiencyGrouping::Profile)
        .unwrap();

    assert_eq!(rollups.len(), 2, "one group per profile label");
    let aggressive = rollups
        .iter()
        .find(|rollup| rollup.profile.as_deref() == Some("aggressive"))
        .unwrap_or_else(|| panic!("aggressive group must exist"));
    assert_eq!(aggressive.input_tokens, 1_000);
    assert_eq!(aggressive.candidate_tokens, Some(100_000));
    assert_eq!(aggressive.selected_tokens, Some(40_000));
    assert!(
        (aggressive
            .savings_ratio
            .unwrap_or_else(|| panic!("measured"))
            - 0.6)
            .abs()
            < 1e-12
    );
    let unprofiled = rollups
        .iter()
        .find(|rollup| rollup.profile.is_none())
        .unwrap_or_else(|| panic!("unprofiled group must exist"));
    assert_eq!(unprofiled.input_tokens, 2_000);
    assert_eq!(unprofiled.savings_ratio, None);
}

/// Old receipt JSON (pre-§13.5.1) deserializes with the documented
/// defaults — the serde back-compat contract pinned in §18.4.
#[test]
fn old_cost_entry_json_without_profile_deserializes() {
    let json = r#"{
        "model": "model-glm",
        "provider": "provider-neuralwatt",
        "agent_domain_id": "agent-backend",
        "role": "implement",
        "project": "p",
        "session": "sess-old",
        "repository": "repo-r",
        "input_tokens": 10,
        "output_tokens": 5,
        "reasoning_tokens": 0,
        "cache_read_tokens": 0,
        "cache_write_tokens": 0,
        "request_count": 1,
        "request_id": "req-old",
        "parent_request_id": null,
        "started_at": "2026-01-01T00:00:00Z",
        "completed_at": "2026-01-01T00:00:01Z",
        "wall_ms": 1000,
        "monetary_cost_measured": null,
        "monetary_cost_estimated": null,
        "monetary_cost_basis": "utilization-only",
        "subscription_attribution_id": null,
        "routing_decision": "bootstrap-default"
    }"#;
    let entry: CostEntry = serde_json::from_str(json).unwrap();
    assert_eq!(entry.profile, None);
}

/// An existing ledger database created before the `profile` column (and
/// without the receipts table) must open and accept profiled entries: the
/// in-place migration runs on open, never loses rows.
#[test]
fn legacy_ledger_migrates_profile_column_in_place() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("cost.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE cost_entries (
              request_id TEXT PRIMARY KEY, model TEXT NOT NULL, provider TEXT NOT NULL, agent_domain_id TEXT NOT NULL, role TEXT NOT NULL, project TEXT NOT NULL, session TEXT NOT NULL, repository TEXT NOT NULL,
              input_tokens INTEGER NOT NULL, output_tokens INTEGER NOT NULL, reasoning_tokens INTEGER NOT NULL, cache_read_tokens INTEGER NOT NULL, cache_write_tokens INTEGER NOT NULL, request_count INTEGER NOT NULL,
              parent_request_id TEXT, started_at TEXT NOT NULL, completed_at TEXT NOT NULL, wall_ms INTEGER NOT NULL, monetary_cost_measured REAL, monetary_cost_estimated REAL, monetary_cost_basis TEXT NOT NULL,
              subscription_attribution_id TEXT, routing_decision TEXT NOT NULL
            );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO cost_entries (request_id, model, provider, agent_domain_id, role, project, session, repository, input_tokens, output_tokens, reasoning_tokens, cache_read_tokens, cache_write_tokens, request_count, parent_request_id, started_at, completed_at, wall_ms, monetary_cost_measured, monetary_cost_estimated, monetary_cost_basis, subscription_attribution_id, routing_decision) VALUES ('legacy', 'm', 'p', 'a', 'r', 'proj', 'sess-legacy', 'repo', 5, 1, 0, 0, 0, 1, NULL, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 0, NULL, NULL, 'utilization-only', NULL, 'bootstrap-default')",
            [],
        )
        .unwrap();
    }
    let ledger = CostLedger::open(&db_path).unwrap();
    // The legacy row survives the migration and rolls up.
    let rollups = ledger
        .token_efficiency_rollups(EfficiencyGrouping::Session)
        .unwrap();
    assert_eq!(rollups.len(), 1);
    assert_eq!(rollups[0].session.as_deref(), Some("sess-legacy"));
    assert_eq!(rollups[0].input_tokens, 5);
    // New profiled entries insert into the migrated table.
    let mut profiled = cost_entry_with_session("sess-new", "req-new", 7, 2);
    profiled.profile = Some(String::from("aggressive"));
    ledger.api_spend().insert_cost_entry(&profiled).unwrap();
    let rollups = ledger
        .token_efficiency_rollups(EfficiencyGrouping::Profile)
        .unwrap();
    assert_eq!(rollups.len(), 2);
    let aggressive = rollups
        .iter()
        .find(|rollup| rollup.profile.as_deref() == Some("aggressive"))
        .unwrap_or_else(|| panic!("aggressive group must exist"));
    assert_eq!(
        aggressive.session, None,
        "profile grouping drops the session key"
    );
    assert_eq!(aggressive.input_tokens, 7);
}
