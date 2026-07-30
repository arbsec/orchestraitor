//! Event-driven input handling (spec §13.3: no polling).
//!
//! Input arrives via crossterm's `EventStream`, which is async and
//! event-driven. The TUI never polls on a timer. Data updates from the
//! daemon arrive via tokio channels. Rendering is triggered only by
//! incoming events, never by a background tick.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEventKind};

use crate::app::{App, NavigationDirection};
use crate::views::ViewId;

/// A decoded TUI input event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TuiEvent {
    /// A keyboard key was pressed.
    Key(KeyCode, KeyModifiers),
    /// A mouse event occurred.
    Mouse(MouseEventKind),
    /// The terminal was resized.
    Resize(u16, u16),
    /// The event stream produced no event (timeout or spurious wake).
    Tick,
}

/// Converts a crossterm `KeyEvent` into a [`TuiEvent`].
#[must_use]
pub fn from_key_event(key: KeyEvent) -> TuiEvent {
    TuiEvent::Key(key.code, key.modifiers)
}

/// Applies a [`TuiEvent`] to the [`App`], mutating its state.
pub fn apply_event(app: &mut App, event: &TuiEvent) {
    match event {
        TuiEvent::Key(code, modifiers) => apply_key(app, *code, *modifiers),
        TuiEvent::Mouse(_) | TuiEvent::Resize(_, _) | TuiEvent::Tick => {}
    }
}

/// Applies a keyboard event to the app.
fn apply_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    match (code, modifiers) {
        (KeyCode::Char('q'), _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) => app.quit(),
        (KeyCode::Tab | KeyCode::Right, _) => {
            app.navigate(NavigationDirection::Next);
        }
        (KeyCode::BackTab | KeyCode::Left, _) => {
            app.navigate(NavigationDirection::Previous);
        }
        (KeyCode::Down, _) => app.scroll_down(),
        (KeyCode::Up, _) => app.scroll_up(),
        // Number keys 1-9 jump to the corresponding view.
        (KeyCode::Char(c @ '1'..='9'), _) => {
            let idx = (c as usize) - ('1' as usize);
            if let Some(view) = ViewId::ALL.get(idx) {
                app.switch_to(*view);
            }
        }
        _ => {}
    }
}

/// A type-erased input source for the TUI.
///
/// In production this wraps crossterm's `EventStream`. In tests it is
/// replaced by a direct channel or a `TestBackend` event list.
pub trait TuiInput {
    /// Returns the next event, or `None` if the stream is closed.
    ///
    /// # Errors
    ///
    /// Returns [`crate::TuiError`] when the underlying stream fails.
    fn next_event(&mut self) -> crate::TuiResult<Option<TuiEvent>>;
}
