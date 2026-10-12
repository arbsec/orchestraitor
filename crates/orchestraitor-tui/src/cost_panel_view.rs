//! Rendering for the cost, subscription, and routing panels (spec §9.19.7).

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};

use crate::cost_panel::{
    CostPanelData, CostPanelRow, RoutingPanelEntry, RoutingPanelEntryKind, SubscriptionPanelRow,
};
use orchestraitor_cost_ledger::UtilizationLabel;

/// Renders the cost panel with cost rows, subscription meters, and routing entries.
pub fn render_cost_panel(frame: &mut Frame<'_>, area: Rect, data: &CostPanelData) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(40),
            Constraint::Percentage(30),
            Constraint::Percentage(30),
        ])
        .split(area);
    render_cost_rows(frame, chunks[0], &data.rows);
    render_subscription_meters(frame, chunks[1], &data.subscriptions);
    render_routing_entries(frame, chunks[2], &data.routing_entries);
}

fn render_cost_rows(frame: &mut Frame<'_>, area: Rect, rows: &[CostPanelRow]) {
    if rows.is_empty() {
        let para =
            Paragraph::new("  (no cost entries)").style(Style::default().fg(Color::DarkGray));
        frame.render_widget(para, area);
        return;
    }
    let items: Vec<ListItem<'_>> = rows
        .iter()
        .map(|row| {
            let cost = row
                .monetary_cost
                .map_or("—".to_string(), |c| format!("${c:.4}"));
            let label = Span::styled(
                format!(" [{}] ", row.basis_label()),
                Style::default().fg(Color::Yellow),
            );
            ListItem::new(Line::from(vec![
                label,
                Span::raw(format!(
                    "{:<40} {:>10}  {:>8} tokens",
                    row.label, cost, row.tokens
                )),
            ]))
        })
        .collect();
    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title("Cost entries [measured/estimated/user-configured]"),
    );
    frame.render_widget(list, area);
}

fn render_subscription_meters(frame: &mut Frame<'_>, area: Rect, subs: &[SubscriptionPanelRow]) {
    if subs.is_empty() {
        let para =
            Paragraph::new("  (no subscriptions)").style(Style::default().fg(Color::DarkGray));
        frame.render_widget(para, area);
        return;
    }
    let items: Vec<ListItem<'_>> = subs
        .iter()
        .map(|sub| {
            let pct = sub
                .utilization_percent()
                .map_or("—".to_string(), |p| format!("{p:.1}%"));
            let label_color = match sub.label {
                UtilizationLabel::Measured => Color::Green,
                UtilizationLabel::Estimated => Color::Yellow,
                UtilizationLabel::UserConfigured => Color::Cyan,
            };
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!(" [{}] ", sub.label_text()),
                    Style::default().fg(label_color),
                ),
                Span::raw(format!(
                    "{:<30} {:>8} / {:>8} tokens  ({})",
                    sub.subscription_id,
                    sub.consumed_tokens,
                    sub.quota_tokens.unwrap_or(0),
                    pct
                )),
            ]))
        })
        .collect();
    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title("Subscription utilization [measured/estimated/user-configured]"),
    );
    frame.render_widget(list, area);
}

fn render_routing_entries(frame: &mut Frame<'_>, area: Rect, entries: &[RoutingPanelEntry]) {
    if entries.is_empty() {
        let para =
            Paragraph::new("  (no routing events)").style(Style::default().fg(Color::DarkGray));
        frame.render_widget(para, area);
        return;
    }
    let items: Vec<ListItem<'_>> = entries
        .iter()
        .map(|entry| {
            let (icon, color) = match entry.kind {
                RoutingPanelEntryKind::RoutingEvent => ("→", Color::White),
                RoutingPanelEntryKind::SoftCapWarning => ("⚠", Color::Yellow),
                RoutingPanelEntryKind::HardCapWarning => ("⛔", Color::Red),
            };
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{icon} "),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::raw(entry.text.as_str()),
            ]))
        })
        .collect();
    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title("Routing events & cap warnings"),
    );
    frame.render_widget(list, area);
}
