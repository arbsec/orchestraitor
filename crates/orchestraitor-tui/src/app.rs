//! App state, view switching, and keyboard navigation (spec §9.2, §13.3).
//!
//! The [`App`] holds the mutable TUI state. Views receive an [`AppSnapshot`]
//! (a read-only projection) and never mutate state directly. Keyboard
//! navigation cycles through all views in [`ViewId::ALL`] order.

use crate::approval::ApprovalData;
use crate::cost_panel::CostPanelData;
use crate::startup::StartupState;
use crate::views::ViewId;

/// Direction for view navigation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavigationDirection {
    /// Move to the next view in the ring.
    Next,
    /// Move to the previous view in the ring.
    Previous,
}

/// Read-only snapshot of app state passed to view renderers.
#[derive(Debug, Clone)]
pub struct AppSnapshot {
    /// Currently active view.
    pub current_view: ViewId,
    /// Cost panel data for rendering.
    pub cost_panel: CostPanelData,
    /// Pending approval requests.
    pub approvals: Vec<ApprovalData>,
    /// Session log lines.
    pub session_logs: Vec<String>,
    /// Changed file paths.
    pub changed_files: Vec<String>,
    /// Security findings.
    pub security_findings: Vec<String>,
    /// Receipt summaries.
    pub receipts: Vec<String>,
    /// Policy trace entries.
    pub policy_trace: Vec<String>,
    /// Context trace entries.
    pub context_trace: Vec<String>,
    /// Tool call summaries.
    pub tool_calls: Vec<String>,
    /// Test/build result summaries.
    pub test_build_results: Vec<String>,
    /// Diff lines (unified format).
    pub diff_lines: Vec<String>,
    /// Active agents per session.
    pub active_agents: Vec<AgentSummary>,
}

/// Summary of an active agent for the Agents view (spec §9.19.7).
#[derive(Debug, Clone, PartialEq)]
pub struct AgentSummary {
    /// Domain identifier.
    pub domain: String,
    /// Role identifier.
    pub role: String,
    /// Provider identifier.
    pub provider: String,
    /// Model identifier.
    pub model: String,
    /// Last cost label (measured/estimated/user-configured).
    pub last_cost_label: String,
}

/// Central TUI application state.
#[derive(Debug, Clone)]
pub struct App {
    /// Currently active view.
    current_view: ViewId,
    /// Startup progress state.
    startup: StartupState,
    /// Cost panel data.
    cost_panel: CostPanelData,
    /// Pending approval requests.
    approvals: Vec<ApprovalData>,
    /// Session log lines.
    session_logs: Vec<String>,
    /// Changed file paths.
    changed_files: Vec<String>,
    /// Security findings.
    security_findings: Vec<String>,
    /// Receipt summaries.
    receipts: Vec<String>,
    /// Policy trace entries.
    policy_trace: Vec<String>,
    /// Context trace entries.
    context_trace: Vec<String>,
    /// Tool call summaries.
    tool_calls: Vec<String>,
    /// Test/build result summaries.
    test_build_results: Vec<String>,
    /// Diff lines (unified format).
    diff_lines: Vec<String>,
    /// Active agents per session.
    active_agents: Vec<AgentSummary>,
    /// Whether the user requested to quit.
    should_quit: bool,
    /// Scroll offset for the current view's list.
    scroll_offset: usize,
}

impl Default for App {
    fn default() -> Self {
        Self {
            current_view: ViewId::Sessions,
            startup: StartupState::default(),
            cost_panel: CostPanelData::default(),
            approvals: Vec::new(),
            session_logs: Vec::new(),
            changed_files: Vec::new(),
            security_findings: Vec::new(),
            receipts: Vec::new(),
            policy_trace: Vec::new(),
            context_trace: Vec::new(),
            tool_calls: Vec::new(),
            test_build_results: Vec::new(),
            diff_lines: Vec::new(),
            active_agents: Vec::new(),
            should_quit: false,
            scroll_offset: 0,
        }
    }
}

impl App {
    /// Creates a new app with default state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the current view.
    #[must_use]
    pub const fn current_view(&self) -> ViewId {
        self.current_view
    }

    /// Returns whether the user requested to quit.
    #[must_use]
    pub const fn should_quit(&self) -> bool {
        self.should_quit
    }

    /// Returns the startup state.
    #[must_use]
    pub const fn startup(&self) -> &StartupState {
        &self.startup
    }

    /// Returns a read-only snapshot for view rendering.
    #[must_use]
    pub fn snapshot(&self) -> AppSnapshot {
        AppSnapshot {
            current_view: self.current_view,
            cost_panel: self.cost_panel.clone(),
            approvals: self.approvals.clone(),
            session_logs: self.session_logs.clone(),
            changed_files: self.changed_files.clone(),
            security_findings: self.security_findings.clone(),
            receipts: self.receipts.clone(),
            policy_trace: self.policy_trace.clone(),
            context_trace: self.context_trace.clone(),
            tool_calls: self.tool_calls.clone(),
            test_build_results: self.test_build_results.clone(),
            diff_lines: self.diff_lines.clone(),
            active_agents: self.active_agents.clone(),
        }
    }

    /// Navigates to the next or previous view.
    pub fn navigate(&mut self, direction: NavigationDirection) {
        self.current_view = match direction {
            NavigationDirection::Next => self.current_view.next(),
            NavigationDirection::Previous => self.current_view.prev(),
        };
        self.scroll_offset = 0;
    }

    /// Switches directly to a specific view.
    pub fn switch_to(&mut self, view: ViewId) {
        self.current_view = view;
        self.scroll_offset = 0;
    }

    /// Scrolls the current view's list down.
    pub fn scroll_down(&mut self) {
        self.scroll_offset = self.scroll_offset.saturating_add(1);
    }

    /// Scrolls the current view's list up.
    pub fn scroll_up(&mut self) {
        self.scroll_offset = self.scroll_offset.saturating_sub(1);
    }

    /// Returns the scroll offset.
    #[must_use]
    pub const fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }

    /// Requests quit.
    pub fn quit(&mut self) {
        self.should_quit = true;
    }

    /// Starts a startup operation.
    pub fn start_startup(&mut self, operation: crate::startup::StartupOperation) {
        self.startup.start(operation);
    }

    /// Finishes the current startup operation.
    pub fn finish_startup(&mut self) {
        self.startup.finish();
    }

    /// Sets cost panel data.
    pub fn set_cost_panel(&mut self, data: CostPanelData) {
        self.cost_panel = data;
    }

    /// Adds an approval request.
    pub fn add_approval(&mut self, approval: ApprovalData) {
        self.approvals.push(approval);
    }

    /// Adds a session log line.
    pub fn add_log(&mut self, line: impl Into<String>) {
        self.session_logs.push(line.into());
    }

    /// Adds a changed file path.
    pub fn add_changed_file(&mut self, path: impl Into<String>) {
        self.changed_files.push(path.into());
    }

    /// Adds a security finding.
    pub fn add_security_finding(&mut self, finding: impl Into<String>) {
        self.security_findings.push(finding.into());
    }

    /// Adds a receipt summary.
    pub fn add_receipt(&mut self, receipt: impl Into<String>) {
        self.receipts.push(receipt.into());
    }

    /// Adds a policy trace entry.
    pub fn add_policy_trace(&mut self, entry: impl Into<String>) {
        self.policy_trace.push(entry.into());
    }

    /// Adds a context trace entry.
    pub fn add_context_trace(&mut self, entry: impl Into<String>) {
        self.context_trace.push(entry.into());
    }

    /// Adds a tool call summary.
    pub fn add_tool_call(&mut self, call: impl Into<String>) {
        self.tool_calls.push(call.into());
    }

    /// Adds a test/build result.
    pub fn add_test_build_result(&mut self, result: impl Into<String>) {
        self.test_build_results.push(result.into());
    }

    /// Sets diff lines.
    pub fn set_diff_lines(&mut self, lines: Vec<String>) {
        self.diff_lines = lines;
    }

    /// Adds an active agent summary.
    pub fn add_agent(&mut self, agent: AgentSummary) {
        self.active_agents.push(agent);
    }
}
