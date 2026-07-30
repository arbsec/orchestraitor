//! Diff view renderer — side-by-side and unified (spec §9.2).
//!
//! Renders unified diff lines with syntax-aware coloring (additions green,
//! deletions red, headers cyan). When the terminal is wide enough, a
//! side-by-side layout is used; otherwise the unified view fills the area.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

/// Minimum width (in columns) for side-by-side layout.
const SIDE_BY_SIDE_MIN_WIDTH: u16 = 80;

/// Renders the diffs view.
pub fn render(frame: &mut Frame<'_>, area: Rect, diff_lines: &[String]) {
    if diff_lines.is_empty() {
        let para = Paragraph::new("  (no diffs)").style(Style::default().fg(Color::DarkGray));
        frame.render_widget(para, area);
        return;
    }
    if area.width >= SIDE_BY_SIDE_MIN_WIDTH {
        render_side_by_side(frame, area, diff_lines);
    } else {
        render_unified(frame, area, diff_lines);
    }
}

/// Renders a unified diff.
fn render_unified(frame: &mut Frame<'_>, area: Rect, diff_lines: &[String]) {
    let lines: Vec<Line<'_>> = diff_lines
        .iter()
        .map(|l| style_diff_line(l.as_str()))
        .collect();
    let para =
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title("Unified diff"));
    frame.render_widget(para, area);
}

/// Renders a side-by-side diff.
fn render_side_by_side(frame: &mut Frame<'_>, area: Rect, diff_lines: &[String]) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);
    let (left, right) = split_diff_sides(diff_lines);
    let left_lines: Vec<Line<'_>> = left.iter().map(|l| style_diff_line(l.as_str())).collect();
    let right_lines: Vec<Line<'_>> = right.iter().map(|l| style_diff_line(l.as_str())).collect();
    let left_para =
        Paragraph::new(left_lines).block(Block::default().borders(Borders::ALL).title("Old"));
    let right_para =
        Paragraph::new(right_lines).block(Block::default().borders(Borders::ALL).title("New"));
    frame.render_widget(left_para, chunks[0]);
    frame.render_widget(right_para, chunks[1]);
}

/// Splits unified diff lines into left (old) and right (new) sides.
fn split_diff_sides(diff_lines: &[String]) -> (Vec<String>, Vec<String>) {
    let mut left = Vec::new();
    let mut right = Vec::new();
    for line in diff_lines {
        if let Some(content) = line.strip_prefix('-') {
            left.push(content.to_string());
        } else if let Some(content) = line.strip_prefix('+') {
            right.push(content.to_string());
        } else if let Some(content) = line.strip_prefix(' ') {
            left.push(content.to_string());
            right.push(content.to_string());
        } else {
            left.push(line.clone());
            right.push(line.clone());
        }
    }
    (left, right)
}

/// Styles a single diff line based on its prefix.
fn style_diff_line(line: &str) -> Line<'static> {
    if line.starts_with("+++") || line.starts_with("---") || line.starts_with("@@") {
        Line::from(Span::styled(
            line.to_string(),
            Style::default().fg(Color::Cyan),
        ))
    } else if line.starts_with('+') {
        Line::from(Span::styled(
            line.to_string(),
            Style::default().fg(Color::Green),
        ))
    } else if line.starts_with('-') {
        Line::from(Span::styled(
            line.to_string(),
            Style::default().fg(Color::Red),
        ))
    } else {
        Line::from(Span::styled(
            line.to_string(),
            Style::default().fg(Color::Gray),
        ))
    }
}
