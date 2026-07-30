//! View identifiers, dispatch, and common rendering helpers (spec §9.2).
//!
//! Every required view from spec §9.2 is an enum variant in [`ViewId`], making
//! exhaustive matching over the view set a compile-time guarantee. The
//! [`render_view`] function dispatches to the appropriate view renderer based
//! on the current view in the snapshot.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{List, ListItem, Paragraph};

use crate::app::AppSnapshot;

/// All reachable TUI views from spec §9.2.
///
/// The order of variants defines the keyboard navigation ring: pressing
/// `Tab` or `Right` from the last view wraps to the first, and `Shift+Tab`
/// or `Left` from the first wraps to the last.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(clippy::enum_variant_names)]
pub enum ViewId {
    /// Active and historical sessions (spec §9.2).
    Sessions,
    /// Agent/harness selection and active agents per session (spec §9.2, §9.19.7).
    Agents,
    /// Token and cost ledger (spec §9.2, §9.19.7).
    CostLedger,
    /// Tool calls issued by agents (spec §9.2).
    ToolCalls,
    /// Arbitraitor approval requests rendered per §9.9.
    Approvals,
    /// Changed files in the active session workspace (spec §9.2).
    ChangedFiles,
    /// Side-by-side and unified diffs (spec §9.2).
    Diffs,
    /// Test and build results (spec §9.2).
    TestBuild,
    /// Security findings from Arbitraitor analysis (spec §9.2).
    SecurityFindings,
    /// Tamper-evident receipts (spec §9.2).
    Receipts,
    /// Session logs and event stream (spec §9.2).
    SessionLogs,
    /// Policy trace from Arbitraitor decisions (spec §9.2).
    PolicyTrace,
    /// Context compiler trace (spec §9.2).
    ContextTrace,
}

impl ViewId {
    /// Returns all view variants in navigation order.
    pub const ALL: &'static [ViewId] = &[
        ViewId::Sessions,
        ViewId::Agents,
        ViewId::CostLedger,
        ViewId::ToolCalls,
        ViewId::Approvals,
        ViewId::ChangedFiles,
        ViewId::Diffs,
        ViewId::TestBuild,
        ViewId::SecurityFindings,
        ViewId::Receipts,
        ViewId::SessionLogs,
        ViewId::PolicyTrace,
        ViewId::ContextTrace,
    ];

    /// Returns the human-readable title for this view.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::Sessions => "Sessions",
            Self::Agents => "Agents",
            Self::CostLedger => "Cost Ledger",
            Self::ToolCalls => "Tool Calls",
            Self::Approvals => "Approvals",
            Self::ChangedFiles => "Changed Files",
            Self::Diffs => "Diffs",
            Self::TestBuild => "Test / Build",
            Self::SecurityFindings => "Security Findings",
            Self::Receipts => "Receipts",
            Self::SessionLogs => "Session Logs",
            Self::PolicyTrace => "Policy Trace",
            Self::ContextTrace => "Context Trace",
        }
    }

    /// Returns the next view in the navigation ring.
    #[must_use]
    pub fn next(self) -> Self {
        let idx = Self::index(self);
        Self::ALL[(idx + 1) % Self::ALL.len()]
    }

    /// Returns the previous view in the navigation ring.
    #[must_use]
    pub fn prev(self) -> Self {
        let idx = Self::index(self);
        let prev_idx = if idx == 0 {
            Self::ALL.len() - 1
        } else {
            idx - 1
        };
        Self::ALL[prev_idx]
    }

    /// Returns the index of this view in [`Self::ALL`].
    fn index(self) -> usize {
        Self::ALL.iter().position(|v| *v == self).unwrap_or(0)
    }
}

/// Renders the current view into the provided frame area.
///
/// This dispatches to the appropriate view renderer based on
/// `snapshot.current_view`.
pub fn render_view(frame: &mut Frame<'_>, area: Rect, snapshot: &AppSnapshot) {
    match snapshot.current_view {
        ViewId::Sessions => render_sessions(frame, area, snapshot),
        ViewId::Agents => render_agents(frame, area, snapshot),
        ViewId::CostLedger => render_cost_ledger(frame, area, snapshot),
        ViewId::ToolCalls => render_tool_calls(frame, area, snapshot),
        ViewId::Approvals => render_approvals(frame, area, snapshot),
        ViewId::ChangedFiles => render_changed_files(frame, area, snapshot),
        ViewId::Diffs => render_diffs(frame, area, snapshot),
        ViewId::TestBuild => render_test_build(frame, area, snapshot),
        ViewId::SecurityFindings => render_security_findings(frame, area, snapshot),
        ViewId::Receipts => render_receipts(frame, area, snapshot),
        ViewId::SessionLogs => render_session_logs(frame, area, snapshot),
        ViewId::PolicyTrace => render_policy_trace(frame, area, snapshot),
        ViewId::ContextTrace => render_context_trace(frame, area, snapshot),
    }
}

/// Renders a list of text items with a scroll offset.
fn render_text_list(frame: &mut Frame<'_>, area: Rect, items: &[String], empty_msg: &str) {
    if items.is_empty() {
        let para = Paragraph::new(format!("  (no {empty_msg})"))
            .style(Style::default().fg(Color::DarkGray));
        frame.render_widget(para, area);
        return;
    }
    let list_items: Vec<ListItem<'_>> = items
        .iter()
        .map(|s| ListItem::new(Line::from(s.as_str())))
        .collect();
    let list = List::new(list_items)
        .style(Style::default().fg(Color::White))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    frame.render_widget(list, area);
}

/// Renders the sessions view.
fn render_sessions(frame: &mut Frame<'_>, area: Rect, snapshot: &AppSnapshot) {
    let items: Vec<String> = if snapshot.active_agents.is_empty() {
        vec!["No active sessions".to_string()]
    } else {
        snapshot
            .active_agents
            .iter()
            .map(|a| format!("{} | {} | {}/{}", a.domain, a.role, a.provider, a.model))
            .collect()
    };
    render_text_list(frame, area, &items, "sessions");
}

/// Renders the agents view (spec §9.19.7).
fn render_agents(frame: &mut Frame<'_>, area: Rect, snapshot: &AppSnapshot) {
    if snapshot.active_agents.is_empty() {
        render_text_list(frame, area, &[], "active agents");
        return;
    }
    let items: Vec<String> = snapshot
        .active_agents
        .iter()
        .map(|a| {
            format!(
                "{:<12} {:<12} {:<16} {:<20} {}",
                a.domain, a.role, a.provider, a.model, a.last_cost_label
            )
        })
        .collect();
    render_text_list(frame, area, &items, "active agents");
}

/// Renders the cost ledger view (spec §9.19.7).
fn render_cost_ledger(frame: &mut Frame<'_>, area: Rect, snapshot: &AppSnapshot) {
    crate::cost_panel_view::render_cost_panel(frame, area, &snapshot.cost_panel);
}

/// Renders the tool calls view.
fn render_tool_calls(frame: &mut Frame<'_>, area: Rect, snapshot: &AppSnapshot) {
    render_text_list(frame, area, &snapshot.tool_calls, "tool calls");
}

/// Renders the approvals view (spec §9.9).
fn render_approvals(frame: &mut Frame<'_>, area: Rect, snapshot: &AppSnapshot) {
    crate::approval_view::render(frame, area, &snapshot.approvals);
}

/// Renders the changed files view.
fn render_changed_files(frame: &mut Frame<'_>, area: Rect, snapshot: &AppSnapshot) {
    render_text_list(frame, area, &snapshot.changed_files, "changed files");
}

/// Renders the diffs view (side-by-side + unified).
fn render_diffs(frame: &mut Frame<'_>, area: Rect, snapshot: &AppSnapshot) {
    crate::diff_view::render(frame, area, &snapshot.diff_lines);
}

/// Renders the test/build results view.
fn render_test_build(frame: &mut Frame<'_>, area: Rect, snapshot: &AppSnapshot) {
    render_text_list(
        frame,
        area,
        &snapshot.test_build_results,
        "test/build results",
    );
}

/// Renders the security findings view.
fn render_security_findings(frame: &mut Frame<'_>, area: Rect, snapshot: &AppSnapshot) {
    render_text_list(
        frame,
        area,
        &snapshot.security_findings,
        "security findings",
    );
}

/// Renders the receipts view.
fn render_receipts(frame: &mut Frame<'_>, area: Rect, snapshot: &AppSnapshot) {
    render_text_list(frame, area, &snapshot.receipts, "receipts");
}

/// Renders the session logs view.
fn render_session_logs(frame: &mut Frame<'_>, area: Rect, snapshot: &AppSnapshot) {
    render_text_list(frame, area, &snapshot.session_logs, "session logs");
}

/// Renders the policy trace view.
fn render_policy_trace(frame: &mut Frame<'_>, area: Rect, snapshot: &AppSnapshot) {
    render_text_list(frame, area, &snapshot.policy_trace, "policy trace entries");
}

/// Renders the context trace view.
fn render_context_trace(frame: &mut Frame<'_>, area: Rect, snapshot: &AppSnapshot) {
    render_text_list(
        frame,
        area,
        &snapshot.context_trace,
        "context trace entries",
    );
}
