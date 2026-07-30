//! Integration tests for the Orchestraitor TUI.
//!
//! These tests use ratatui's `TestBackend` to verify:
//! - All views are reachable via keyboard navigation (spec §9.2).
//! - The startup progress banner renders for ≥200 ms operations (spec §13.3.1).
//! - The cost panel renders `measured` / `estimated` / `user-configured` labels
//!   (spec §9.19.7).
//! - Approval rendering shows all §9.9 fields.

#![allow(clippy::unwrap_used)]

use std::time::{Duration, Instant};

use ratatui::Terminal;
use ratatui::backend::TestBackend;

use crate::app::{App, NavigationDirection};
use crate::approval::{
    ApprovalAction, ApprovalData, ApprovalScope, NetworkDestination, SandboxControl,
    SecretUseIndicator,
};
use crate::cost_panel::{
    CostPanelData, CostPanelRow, RoutingPanelEntry, RoutingPanelEntryKind, SubscriptionPanelRow,
};
use crate::event::{TuiEvent, TuiInput, apply_event};
use crate::layout;
use crate::startup::{StartupOperation, StartupProgress, StartupState};
use crate::views::ViewId;

use orchestraitor_cost_ledger::{MonetaryCostBasis, UtilizationLabel};

/// A test input source that yields pre-loaded events.
struct TestInput {
    events: Vec<TuiEvent>,
    index: usize,
}

impl TestInput {
    fn new(events: Vec<TuiEvent>) -> Self {
        Self { events, index: 0 }
    }
}

impl TuiInput for TestInput {
    fn next_event(&mut self) -> crate::TuiResult<Option<TuiEvent>> {
        if self.index < self.events.len() {
            let event = self.events[self.index].clone();
            self.index += 1;
            Ok(Some(event))
        } else {
            Ok(None)
        }
    }
}

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

/// Checks whether the buffer contains a given substring.
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

// ── Keyboard navigation tests ──────────────────────────────────────────

#[test]
fn all_views_reachable_via_tab_navigation() {
    let mut app = App::new();
    let mut visited = Vec::new();
    visited.push(app.current_view());

    for _ in 0..ViewId::ALL.len() {
        app.navigate(NavigationDirection::Next);
        visited.push(app.current_view());
    }

    let unique: std::collections::HashSet<_> = visited.iter().collect();
    assert_eq!(
        unique.len(),
        ViewId::ALL.len(),
        "Tab navigation should visit all {count} views",
        count = ViewId::ALL.len()
    );
}

#[test]
fn all_views_reachable_via_shift_tab_navigation() {
    let mut app = App::new();
    let mut visited = vec![app.current_view()];

    for _ in 0..ViewId::ALL.len() {
        app.navigate(NavigationDirection::Previous);
        visited.push(app.current_view());
    }

    let unique: std::collections::HashSet<_> = visited.iter().collect();
    assert_eq!(
        unique.len(),
        ViewId::ALL.len(),
        "Shift+Tab navigation should visit all views"
    );
}

#[test]
fn number_keys_jump_to_views() {
    let mut app = App::new();
    let mut input = TestInput::new(vec![TuiEvent::Key(
        crossterm::event::KeyCode::Char('3'),
        crossterm::event::KeyModifiers::NONE,
    )]);
    while let Some(event) = input.next_event().unwrap() {
        apply_event(&mut app, &event);
    }
    assert_eq!(app.current_view(), ViewId::CostLedger);
}

#[test]
fn quit_key_sets_should_quit() {
    let mut app = App::new();
    apply_event(
        &mut app,
        &TuiEvent::Key(
            crossterm::event::KeyCode::Char('q'),
            crossterm::event::KeyModifiers::NONE,
        ),
    );
    assert!(app.should_quit());
}

#[test]
fn ctrl_c_sets_should_quit() {
    let mut app = App::new();
    apply_event(
        &mut app,
        &TuiEvent::Key(
            crossterm::event::KeyCode::Char('c'),
            crossterm::event::KeyModifiers::CONTROL,
        ),
    );
    assert!(app.should_quit());
}

#[test]
fn each_view_renders_without_panic() {
    let mut app = App::new();
    app.add_log("session started");
    app.add_changed_file("src/main.rs");
    app.add_tool_call("read_file(src/main.rs)");
    app.add_test_build_result("cargo test: 3 passed");
    app.add_security_finding("no sandbox bypass detected");
    app.add_receipt("receipt-abc123");
    app.add_policy_trace("policy: allow (rule R1)");
    app.add_context_trace("context: 1200 tokens selected");
    app.set_diff_lines(vec![
        "--- a/file.rs".to_string(),
        "+++ b/file.rs".to_string(),
        "-old line".to_string(),
        "+new line".to_string(),
    ]);

    for _ in 0..ViewId::ALL.len() {
        let terminal = render_app(&app, 120, 40);
        let text = buffer_text(&terminal);
        assert!(
            !text.is_empty(),
            "view {:?} should render content",
            app.current_view()
        );
        app.navigate(NavigationDirection::Next);
    }
}

// ── Startup banner tests (spec §13.3.1) ────────────────────────────────

#[test]
fn startup_banner_hidden_under_200ms() {
    let mut app = App::new();
    app.start_startup(StartupOperation::new("loading MCP servers"));
    let terminal = render_app(&app, 80, 24);
    assert!(
        !buffer_contains(&terminal, "Starting up"),
        "banner should be hidden under 200ms"
    );
}

#[test]
fn startup_banner_visible_after_200ms() {
    let mut app = App::new();
    app.start_startup(StartupOperation::new("loading MCP servers").with_count(4, 12));
    let mut startup = app.startup().clone();
    if let crate::startup::StartupState::InProgress(ref mut progress) = startup {
        progress.started_at = Instant::now()
            .checked_sub(Duration::from_millis(250))
            .unwrap();
    }
    let snapshot = app.snapshot();
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| layout::draw(frame, &snapshot, &startup))
        .unwrap();
    assert!(buffer_contains(&terminal, "Starting up"));
    assert!(buffer_contains(&terminal, "loading MCP servers"));
    assert!(buffer_contains(&terminal, "4 of 12"));
}

#[test]
fn startup_banner_shows_skip_after_1s() {
    let op = StartupOperation::new("models.dev refresh")
        .skippable()
        .with_explanation("using bundled snapshot, prices may be stale");
    let mut progress = StartupProgress::new(op);
    progress.started_at = Instant::now()
        .checked_sub(Duration::from_millis(1100))
        .unwrap();
    let startup = StartupState::InProgress(progress);
    let app = App::new();
    let snapshot = app.snapshot();
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| layout::draw(frame, &snapshot, &startup))
        .unwrap();
    assert!(buffer_contains(&terminal, "skip"));
    assert!(buffer_contains(&terminal, "using bundled snapshot"));
}

#[test]
fn startup_finishes_to_idle() {
    let mut app = App::new();
    app.start_startup(StartupOperation::new("test"));
    app.finish_startup();
    assert!(!app.startup().is_active());
}

// ── Cost panel label tests (spec §9.19.7) ──────────────────────────────

#[test]
fn cost_panel_renders_all_three_labels() {
    let mut app = App::new();
    app.switch_to(ViewId::CostLedger);
    app.set_cost_panel(CostPanelData {
        rows: vec![
            CostPanelRow {
                label: "agent-1 / implementing / openai".to_string(),
                monetary_cost: Some(0.0042),
                cost_basis: MonetaryCostBasis::ProviderMeasured,
                tokens: 1500,
                subscription_label: None,
            },
            CostPanelRow {
                label: "agent-2 / reviewing / anthropic".to_string(),
                monetary_cost: Some(0.0110),
                cost_basis: MonetaryCostBasis::PriceSheetEstimated,
                tokens: 3200,
                subscription_label: None,
            },
            CostPanelRow {
                label: "agent-3 / planning / google".to_string(),
                monetary_cost: Some(0.0500),
                cost_basis: MonetaryCostBasis::UserConfiguredSubscriptionPrice,
                tokens: 800,
                subscription_label: Some("sub-001".to_string()),
            },
        ],
        subscriptions: vec![
            SubscriptionPanelRow {
                subscription_id: "sub-measured".to_string(),
                label: UtilizationLabel::Measured,
                consumed_tokens: 5000,
                quota_tokens: Some(50_000),
                monthly_price_usd: None,
            },
            SubscriptionPanelRow {
                subscription_id: "sub-estimated".to_string(),
                label: UtilizationLabel::Estimated,
                consumed_tokens: 12_000,
                quota_tokens: Some(100_000),
                monthly_price_usd: None,
            },
            SubscriptionPanelRow {
                subscription_id: "sub-user-configured".to_string(),
                label: UtilizationLabel::UserConfigured,
                consumed_tokens: 3_000,
                quota_tokens: Some(30_000),
                monthly_price_usd: Some(20.0),
            },
        ],
        routing_entries: vec![RoutingPanelEntry {
            kind: RoutingPanelEntryKind::RoutingEvent,
            text: "domain_default → openai/gpt-4".to_string(),
        }],
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

#[test]
fn cost_panel_renders_routing_events() {
    let mut app = App::new();
    app.switch_to(ViewId::CostLedger);
    app.set_cost_panel(CostPanelData {
        rows: vec![],
        subscriptions: vec![],
        routing_entries: vec![
            RoutingPanelEntry::routing_event("domain_default", "openai", "gpt-4"),
            RoutingPanelEntry::budget_warning(&orchestraitor_cost_ledger::BudgetWarningEvent {
                scope: orchestraitor_cost_ledger::BudgetScope::Session,
                scope_id: "sess_123".to_string(),
                metric: orchestraitor_cost_ledger::CapMetric::Tokens,
                cap_kind: orchestraitor_cost_ledger::CapKind::Soft,
                projected: 12_000.0,
                cap: 10_000.0,
            }),
        ],
    });
    let terminal = render_app(&app, 120, 40);
    let text = buffer_text(&terminal);
    assert!(
        text.contains("domain_default"),
        "routing event should be visible"
    );
    assert!(text.contains("soft"), "soft cap warning should be visible");
}

// ── Approval rendering tests (spec §9.9) ────────────────────────────────

#[test]
fn approval_view_renders_all_required_fields() {
    let mut app = App::new();
    app.switch_to(ViewId::Approvals);
    app.add_approval(ApprovalData {
        operation: "run cargo test".to_string(),
        action: ApprovalAction::OneAction,
        executable: "/usr/bin/cargo".to_string(),
        arguments: vec!["test".to_string(), "--workspace".to_string()],
        paths: vec!["src/main.rs".to_string(), "src/lib.rs".to_string()],
        network_destinations: vec![NetworkDestination {
            host: "crates.io".to_string(),
            port: Some(443),
            scheme: "https".to_string(),
            purpose: "dependency fetch".to_string(),
        }],
        secret_use: vec![SecretUseIndicator {
            name: "CARGO_REGISTRY_TOKEN".to_string(),
            uri: "secret://env/CARGO_REGISTRY_TOKEN".to_string(),
            purpose: "registry authentication".to_string(),
        }],
        sandbox_controls: vec![SandboxControl {
            name: "landlock".to_string(),
            effective: true,
        }],
        expected_outputs: vec!["test results".to_string()],
        static_findings: vec!["no shell injection".to_string()],
        policy_rule: "R1: allow cargo test in workspace".to_string(),
        scope: ApprovalScope {
            description: "single action, session-scoped".to_string(),
            expiry: None,
        },
        affects_host_trusted_state: false,
        agent_prose: Some("I need to run tests to verify my changes.".to_string()),
    });

    let terminal = render_app(&app, 120, 40);
    let text = buffer_text(&terminal);
    assert!(text.contains("run cargo test"), "operation must be shown");
    assert!(text.contains("/usr/bin/cargo"), "executable must be shown");
    assert!(
        text.contains("CARGO_REGISTRY_TOKEN"),
        "secret name must be shown"
    );
    assert!(text.contains("landlock"), "sandbox control must be shown");
    assert!(text.contains("R1"), "policy rule must be shown");
    assert!(
        text.contains("UNTRUSTED"),
        "agent prose must be marked untrusted"
    );
}

#[test]
fn approval_view_shows_host_trusted_warning() {
    let mut app = App::new();
    app.switch_to(ViewId::Approvals);
    app.add_approval(ApprovalData {
        operation: "write to host .git".to_string(),
        action: ApprovalAction::GitCapability,
        executable: "git".to_string(),
        arguments: vec!["commit".to_string()],
        paths: vec![".git/HEAD".to_string()],
        network_destinations: vec![],
        secret_use: vec![],
        sandbox_controls: vec![],
        expected_outputs: vec![],
        static_findings: vec![],
        policy_rule: "R2: deny host git writes".to_string(),
        scope: ApprovalScope {
            description: "repository-scoped".to_string(),
            expiry: Some("2026-07-30T12:00:00Z".to_string()),
        },
        affects_host_trusted_state: true,
        agent_prose: None,
    });
    let terminal = render_app(&app, 120, 40);
    let text = buffer_text(&terminal);
    assert!(
        text.contains("host-trusted"),
        "host-trusted state warning must be shown"
    );
}

// ── Diff view tests ─────────────────────────────────────────────────────

#[test]
fn diff_view_renders_unified_diff() {
    let mut app = App::new();
    app.switch_to(ViewId::Diffs);
    app.set_diff_lines(vec![
        "--- a/file.rs".to_string(),
        "+++ b/file.rs".to_string(),
        "-old line".to_string(),
        "+new line".to_string(),
        " context line".to_string(),
    ]);
    let terminal = render_app(&app, 60, 20);
    let text = buffer_text(&terminal);
    assert!(text.contains("file.rs"), "diff should show filename");
}

#[test]
fn diff_view_renders_side_by_side_when_wide() {
    let mut app = App::new();
    app.switch_to(ViewId::Diffs);
    app.set_diff_lines(vec![
        "--- a/file.rs".to_string(),
        "+++ b/file.rs".to_string(),
        "-old line".to_string(),
        "+new line".to_string(),
    ]);
    let terminal = render_app(&app, 100, 20);
    let text = buffer_text(&terminal);
    assert!(text.contains("Old"), "side-by-side should show 'Old' pane");
    assert!(text.contains("New"), "side-by-side should show 'New' pane");
}
