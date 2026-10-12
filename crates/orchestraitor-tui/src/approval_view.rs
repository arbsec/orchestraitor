//! Approval view renderer (spec §9.9).
//!
//! Renders Arbitraitor-provided structured approval data. The TUI never
//! constructs or validates approval tokens. Per spec §9.9, the UI must show:
//! operation, executable identity, arguments, paths, network destinations,
//! secret use without secret value, sandbox controls, expected outputs,
//! static findings, policy rule, scope and expiry, and whether the action
//! affects host-trusted state. Agent prose is shown separately and marked
//! untrusted.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};

use crate::approval::ApprovalData;

/// Renders the approvals view.
pub fn render(frame: &mut Frame<'_>, area: Rect, approvals: &[ApprovalData]) {
    if approvals.is_empty() {
        let para =
            Paragraph::new("  (no pending approvals)").style(Style::default().fg(Color::DarkGray));
        frame.render_widget(para, area);
        return;
    }
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(area);
    render_approval_list(frame, chunks[0], approvals);
    render_approval_detail(frame, chunks[1], &approvals[0]);
}

/// Renders the list of pending approvals.
fn render_approval_list(frame: &mut Frame<'_>, area: Rect, approvals: &[ApprovalData]) {
    let items: Vec<ListItem<'_>> = approvals
        .iter()
        .map(|a| {
            ListItem::new(Line::from(vec![
                Span::styled(a.action.label(), Style::default().fg(Color::Yellow)),
                Span::raw("  "),
                Span::raw(a.operation.as_str()),
            ]))
        })
        .collect();
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("Pending"))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    frame.render_widget(list, area);
}

/// Renders the detail of a single approval (spec §9.9 fields).
fn render_approval_detail(frame: &mut Frame<'_>, area: Rect, approval: &ApprovalData) {
    let mut lines = Vec::new();
    push_field(&mut lines, "Operation", &approval.operation);
    push_field(&mut lines, "Action", approval.action.label());
    push_field(&mut lines, "Executable", &approval.executable);
    push_field(&mut lines, "Arguments", &approval.arguments.join(" "));
    push_field(&mut lines, "Paths", &approval.paths.join(", "));
    push_field(
        &mut lines,
        "Network",
        &format_network(&approval.network_destinations),
    );
    push_field(&mut lines, "Secrets", &format_secrets(&approval.secret_use));
    push_field(
        &mut lines,
        "Sandbox",
        &format_sandbox(&approval.sandbox_controls),
    );
    push_field(
        &mut lines,
        "Expected outputs",
        &approval.expected_outputs.join(", "),
    );
    push_field(
        &mut lines,
        "Static findings",
        &approval.static_findings.join(", "),
    );
    push_field(&mut lines, "Policy rule", &approval.policy_rule);
    push_field(&mut lines, "Scope", &approval.scope.description);
    if let Some(expiry) = &approval.scope.expiry {
        push_field(&mut lines, "Expiry", expiry);
    }
    let host_label = if approval.affects_host_trusted_state {
        Span::styled(
            "YES — host-trusted state affected",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled("no", Style::default().fg(Color::Green))
    };
    lines.push(Line::from(vec![Span::raw("Host-trusted: "), host_label]));
    if let Some(prose) = &approval.agent_prose {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled(
                "⚠ Agent prose (UNTRUSTED): ",
                Style::default().fg(Color::Red),
            ),
            Span::raw(prose.as_str()),
        ]));
    }
    let para = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title("Approval detail"),
    );
    frame.render_widget(para, area);
}

fn push_field(lines: &mut Vec<Line<'_>>, label: &str, value: &str) {
    lines.push(Line::from(vec![
        Span::styled(format!("{label}: "), Style::default().fg(Color::Cyan)),
        Span::raw(value.to_string()),
    ]));
}

fn format_network(dests: &[crate::approval::NetworkDestination]) -> String {
    if dests.is_empty() {
        return "(none)".to_string();
    }
    dests
        .iter()
        .map(|d| format!("{}://{}:{}", d.scheme, d.host, d.port.unwrap_or(0)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn format_secrets(secrets: &[crate::approval::SecretUseIndicator]) -> String {
    if secrets.is_empty() {
        return "(none)".to_string();
    }
    secrets
        .iter()
        .map(|s| format!("{} ({})", s.name, s.purpose))
        .collect::<Vec<_>>()
        .join(", ")
}

fn format_sandbox(controls: &[crate::approval::SandboxControl]) -> String {
    if controls.is_empty() {
        return "(none reported)".to_string();
    }
    controls
        .iter()
        .map(|c| {
            let status = if c.effective { "✓" } else { "✗" };
            format!("{status} {}", c.name)
        })
        .collect::<Vec<_>>()
        .join(", ")
}
