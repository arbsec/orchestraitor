//! Ratatui reference TUI client for Orchestraitor (spec §9.2).
//!
//! This crate implements the first-class TUI client: a multi-pane, keyboard-first,
//! mouse-capable terminal interface that renders sessions, agents, cost ledgers,
//! approvals, diffs, receipts, and all other required views from spec §9.2.
//!
//! ## Design invariants
//!
//! - **Event-driven, never polling** (spec §13.3): input arrives via crossterm's
//!   `EventStream`, and data updates arrive via tokio channels. No background
//!   timers or poll loops drive rendering.
//! - **Startup feedback** (spec §13.3.1): any operation expected to take ≥200 ms
//!   shows a single-line progress banner with an indeterminate indicator, elapsed
//!   time, and item count before the main layout takes over.
//! - **Approval rendering** (spec §9.9): the TUI renders trusted views from
//!   Arbitraitor-provided structured data. It never constructs or validates
//!   approval tokens — that is Arbitraitor's exclusive responsibility.
//! - **Cost labels** (spec §9.19.7): every cost and subscription figure carries a
//!   `measured`, `estimated`, or `user-configured` label so the user always knows
//!   the confidence basis.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod app;
pub mod approval;
pub mod approval_view;
pub mod cost_panel;
pub mod cost_panel_view;
pub mod diff_view;
pub mod error;
pub mod event;
pub mod layout;
pub mod startup;
pub mod views;

pub use app::{App, AppSnapshot, NavigationDirection};
pub use approval::{
    ApprovalAction, ApprovalData, ApprovalScope, NetworkDestination, SandboxControl,
    SecretUseIndicator,
};
pub use cost_panel::{
    CostPanelData, CostPanelRow, RoutingPanelEntry, RoutingPanelEntryKind, SubscriptionPanelRow,
};
pub use error::{TuiError, TuiResult};
pub use event::{TuiEvent, TuiInput};
pub use layout::{PaneKind, TuiLayout};
pub use startup::{StartupOperation, StartupProgress, StartupState};
pub use views::ViewId;

#[cfg(test)]
mod tests;
