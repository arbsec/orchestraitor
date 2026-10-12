//! Multi-pane layout renderer (spec §9.2, §13.3).
//!
//! The layout is keyboard-first but mouse-capable. It consists of:
//! - A top tab bar listing all views from [`ViewId::ALL`].
//! - A main content area rendering the active view.
//! - A bottom status bar with keybindings and the current view title.
//!
//! When a startup operation is in progress (spec §13.3.1), the startup
//! banner replaces the main content area until the operation completes.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Tabs};

use crate::app::AppSnapshot;
use crate::startup::StartupState;
use crate::views::ViewId;

/// Identifies a pane in the multi-pane layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneKind {
    /// The top tab bar.
    TabBar,
    /// The main content area.
    Content,
    /// The bottom status bar.
    StatusBar,
}

/// The TUI layout renderer.
#[derive(Debug, Clone)]
pub struct TuiLayout {
    area: Rect,
}

impl TuiLayout {
    /// Creates a new layout for the given terminal area.
    #[must_use]
    pub const fn new(area: Rect) -> Self {
        Self { area }
    }

    /// Returns the area for the given pane.
    #[must_use]
    pub fn pane_area(&self, kind: PaneKind) -> Rect {
        let chunks = self.split();
        match kind {
            PaneKind::TabBar => chunks[0],
            PaneKind::Content => chunks[1],
            PaneKind::StatusBar => chunks[2],
        }
    }

    /// Splits the layout area into three vertical chunks.
    fn split(&self) -> Vec<Rect> {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(1),
                Constraint::Length(1),
            ])
            .split(self.area)
            .to_vec()
    }
}

/// Draws the full TUI layout into the frame.
pub fn draw(frame: &mut Frame<'_>, snapshot: &AppSnapshot, startup: &StartupState) {
    let area = frame.area();
    let layout = TuiLayout::new(area);

    draw_tab_bar(
        frame,
        layout.pane_area(PaneKind::TabBar),
        snapshot.current_view,
    );

    if startup.is_active() {
        draw_startup_banner(frame, layout.pane_area(PaneKind::Content), startup);
    } else {
        draw_content(frame, layout.pane_area(PaneKind::Content), snapshot);
    }

    draw_status_bar(
        frame,
        layout.pane_area(PaneKind::StatusBar),
        snapshot.current_view,
    );
}

/// Draws the tab bar with all view titles.
fn draw_tab_bar(frame: &mut Frame<'_>, area: Rect, current: ViewId) {
    let titles: Vec<Line<'_>> = ViewId::ALL
        .iter()
        .map(|v| {
            let style = if *v == current {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            Line::from(Span::styled(v.title(), style))
        })
        .collect();
    let tabs = Tabs::new(titles)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Orchestraitor"),
        )
        .select(ViewId::ALL.iter().position(|v| *v == current).unwrap_or(0));
    frame.render_widget(tabs, area);
}

/// Draws the main content area for the current view.
fn draw_content(frame: &mut Frame<'_>, area: Rect, snapshot: &AppSnapshot) {
    let title = snapshot.current_view.title();
    let block = Block::default().borders(Borders::ALL).title(title);
    frame.render_widget(block, area);
    let inner = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };
    crate::views::render_view(frame, inner, snapshot);
}

/// Draws the startup progress banner (spec §13.3.1).
fn draw_startup_banner(frame: &mut Frame<'_>, area: Rect, startup: &StartupState) {
    let Some(progress) = startup.progress() else {
        return;
    };
    let op = &progress.operation;
    let elapsed_ms = progress.elapsed().as_millis();
    let mut lines = Vec::new();

    let header = Line::from(vec![
        Span::styled("⟳ ", Style::default().fg(Color::Cyan)),
        Span::styled(
            op.name.as_str(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!("  ({elapsed_ms} ms)")),
    ]);
    lines.push(header);

    if let Some(count) = op.count_text() {
        lines.push(Line::from(format!("  {count}")));
    }
    if progress.should_offer_skip() {
        if let Some(explanation) = &op.explanation {
            lines.push(Line::from(format!("  ⚠ {explanation}")));
        }
        lines.push(Line::from("  Press 's' to skip"));
    }

    let paragraph =
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title("Starting up"));
    frame.render_widget(paragraph, area);
}

/// Draws the bottom status bar with keybindings.
fn draw_status_bar(frame: &mut Frame<'_>, area: Rect, current: ViewId) {
    let hint = format!(
        " {} | Tab/← →: switch | ↑↓: scroll | 1-9: jump | q: quit",
        current.title()
    );
    let bar = Paragraph::new(hint).style(Style::default().fg(Color::Black).bg(Color::DarkGray));
    frame.render_widget(bar, area);
}
