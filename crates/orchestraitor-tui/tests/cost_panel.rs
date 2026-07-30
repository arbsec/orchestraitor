//! Integration tests for the TUI cost panel rendering (spec §9.19.7).
//!
//! These tests render the cost ledger view through the full public TUI
//! pipeline — `App` → `AppSnapshot` → `layout::draw` → `TestBackend` — and
//! assert that the `measured` / `estimated` / `user-configured` labels,
//! subscription utilization meters, cap warnings, and routing events are
//! visible in the rendered buffer.

#![allow(clippy::unwrap_used)]

use ratatui::Terminal;
use ratatui::backend::TestBackend;

use orchestraitor_cost_ledger::{
    BudgetScope, BudgetWarningEvent, CapKind, CapMetric, MonetaryCostBasis, UtilizationLabel,
};
use orchestraitor_tui::layout;
use orchestraitor_tui::{
    App, CostPanelData, CostPanelRow, RoutingPanelEntry, SubscriptionPanelRow, ViewId,
};

/// Extracts all text from a `TestBackend` buffer as a single string.
fn buffer_text(terminal: &Terminal<TestBackend>) -> String {
    let mut text = String::new();
    let area = terminal.backend().buffer().area;
    for y in 0..area.height {
        for x in 0..area.width {
            let cell = terminal.backend().buffer().cell((x, y)).unwrap();
            text.push(cell.symbol().chars().next().unwrap_or(' '));
        }
        text.push('\n');
    }
    text
}

/// Checks whether the rendered buffer contains a given substring.
fn buffer_contains(terminal: &Terminal<TestBackend>, needle: &str) -> bool {
    buffer_text(terminal).contains(needle)
}

/// Renders the current app state into a `TestBackend` terminal.
fn render_app(app: &App, width: u16, height: u16) -> Terminal<TestBackend> {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    let snapshot = app.snapshot();
    terminal
        .draw(|frame| layout::draw(frame, &snapshot, app.startup()))
        .unwrap();
    terminal
}

// ── Cost basis labels (spec §9.19.7) ──────────────────────────────────

/// The cost panel renders all three cost-basis labels: `measured`,
/// `estimated`, and `user-configured`.
#[test]
fn cost_panel_renders_all_three_cost_basis_labels() {
    let mut app = App::new();
    app.switch_to(ViewId::CostLedger);
    app.set_cost_panel(CostPanelData {
        rows: vec![
            CostPanelRow {
                label: "backend / implementing / neuralwatt".to_string(),
                monetary_cost: Some(0.0042),
                cost_basis: MonetaryCostBasis::ProviderMeasured,
                tokens: 1500,
                subscription_label: None,
            },
            CostPanelRow {
                label: "frontend / reviewing / anthropic".to_string(),
                monetary_cost: Some(0.0110),
                cost_basis: MonetaryCostBasis::PriceSheetEstimated,
                tokens: 3200,
                subscription_label: None,
            },
            CostPanelRow {
                label: "general / planning / google".to_string(),
                monetary_cost: Some(0.0500),
                cost_basis: MonetaryCostBasis::UserConfiguredSubscriptionPrice,
                tokens: 800,
                subscription_label: Some("sub-001".to_string()),
            },
        ],
        subscriptions: vec![],
        routing_entries: vec![],
    });

    let terminal = render_app(&app, 120, 40);
    let text = buffer_text(&terminal);

    assert!(
        text.contains("measured"),
        "cost panel must show 'measured' label"
    );
    assert!(
        text.contains("estimated"),
        "cost panel must show 'estimated' label"
    );
    assert!(
        text.contains("user-configured"),
        "cost panel must show 'user-configured' label"
    );
}

// ── Subscription utilization labels (spec §9.19.7) ────────────────────

/// The cost panel renders subscription utilization meters with the
/// `measured`, `estimated`, and `user-configured` labels.
#[test]
fn cost_panel_renders_all_three_subscription_utilization_labels() {
    let mut app = App::new();
    app.switch_to(ViewId::CostLedger);
    app.set_cost_panel(CostPanelData {
        rows: vec![],
        subscriptions: vec![
            SubscriptionPanelRow {
                subscription_id: "neuralwatt-monthly".to_string(),
                label: UtilizationLabel::Measured,
                consumed_tokens: 5_000,
                quota_tokens: Some(50_000_000),
                monthly_price_usd: None,
            },
            SubscriptionPanelRow {
                subscription_id: "zai-monthly".to_string(),
                label: UtilizationLabel::Estimated,
                consumed_tokens: 12_000,
                quota_tokens: Some(100_000_000),
                monthly_price_usd: None,
            },
            SubscriptionPanelRow {
                subscription_id: "copilot-monthly".to_string(),
                label: UtilizationLabel::UserConfigured,
                consumed_tokens: 3_000,
                quota_tokens: Some(30_000),
                monthly_price_usd: Some(20.0),
            },
        ],
        routing_entries: vec![],
    });

    let terminal = render_app(&app, 120, 40);
    let text = buffer_text(&terminal);

    assert!(
        text.contains("measured"),
        "subscription panel must show 'measured' label"
    );
    assert!(
        text.contains("estimated"),
        "subscription panel must show 'estimated' label"
    );
    assert!(
        text.contains("user-configured"),
        "subscription panel must show 'user-configured' label"
    );
    assert!(
        text.contains("neuralwatt-monthly"),
        "subscription id must be visible"
    );
}

// ── Cap warnings (spec §9.19.5, §9.19.7) ─────────────────────────────

/// The cost panel renders soft-cap and hard-cap warnings.
#[test]
fn cost_panel_renders_soft_and_hard_cap_warnings() {
    let mut app = App::new();
    app.switch_to(ViewId::CostLedger);
    app.set_cost_panel(CostPanelData {
        rows: vec![],
        subscriptions: vec![],
        routing_entries: vec![
            RoutingPanelEntry::budget_warning(&BudgetWarningEvent {
                scope: BudgetScope::Domain,
                scope_id: "backend".to_string(),
                metric: CapMetric::Tokens,
                cap_kind: CapKind::Soft,
                projected: 12_000.0,
                cap: 10_000.0,
            }),
            RoutingPanelEntry::budget_warning(&BudgetWarningEvent {
                scope: BudgetScope::Domain,
                scope_id: "backend".to_string(),
                metric: CapMetric::Tokens,
                cap_kind: CapKind::Hard,
                projected: 55_000.0,
                cap: 50_000.0,
            }),
        ],
    });

    let terminal = render_app(&app, 120, 40);
    let text = buffer_text(&terminal);

    assert!(text.contains("soft"), "soft cap warning must be visible");
    assert!(text.contains("hard"), "hard cap warning must be visible");
    assert!(
        text.contains("backend"),
        "scope id must be visible in warning"
    );
}

// ── Routing events (spec §9.19.7) ─────────────────────────────────────

/// The cost panel renders live per-call model-routing events showing which
/// precedence step matched and the selected provider/model.
#[test]
fn cost_panel_renders_routing_events() {
    let mut app = App::new();
    app.switch_to(ViewId::CostLedger);
    app.set_cost_panel(CostPanelData {
        rows: vec![],
        subscriptions: vec![],
        routing_entries: vec![
            RoutingPanelEntry::routing_event("domain_default", "neuralwatt", "glm-5.2"),
            RoutingPanelEntry::routing_event("project_override", "openai", "gpt-4"),
        ],
    });

    let terminal = render_app(&app, 120, 40);
    let text = buffer_text(&terminal);

    assert!(
        text.contains("domain_default"),
        "routing event must show matched precedence step"
    );
    assert!(
        text.contains("neuralwatt"),
        "routing event must show selected provider"
    );
    assert!(
        text.contains("glm-5.2"),
        "routing event must show selected model"
    );
}

// ── Empty state (spec §9.19.7) ────────────────────────────────────────

/// The cost panel renders placeholder text when there is no data.
#[test]
fn cost_panel_renders_empty_state_placeholders() {
    let mut app = App::new();
    app.switch_to(ViewId::CostLedger);
    app.set_cost_panel(CostPanelData::default());

    let terminal = render_app(&app, 120, 40);

    assert!(
        buffer_contains(&terminal, "no cost entries"),
        "empty cost panel must show placeholder"
    );
    assert!(
        buffer_contains(&terminal, "no subscriptions"),
        "empty subscription panel must show placeholder"
    );
    assert!(
        buffer_contains(&terminal, "no routing events"),
        "empty routing panel must show placeholder"
    );
}

// ── Utilization percentage (spec §9.19.7) ────────────────────────────

/// The cost panel renders the utilization percentage for subscriptions
/// with a known quota.
#[test]
fn cost_panel_renders_subscription_utilization_percentage() {
    let mut app = App::new();
    app.switch_to(ViewId::CostLedger);
    app.set_cost_panel(CostPanelData {
        rows: vec![],
        subscriptions: vec![SubscriptionPanelRow {
            subscription_id: "neuralwatt-monthly".to_string(),
            label: UtilizationLabel::Measured,
            consumed_tokens: 25_000,
            quota_tokens: Some(50_000),
            monthly_price_usd: None,
        }],
        routing_entries: vec![],
    });

    let terminal = render_app(&app, 120, 40);
    let text = buffer_text(&terminal);

    assert!(
        text.contains("50.0%"),
        "subscription panel must show 50.0% utilization (25000 / 50000)"
    );
}
