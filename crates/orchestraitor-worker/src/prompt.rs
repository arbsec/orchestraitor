//! The bootstrap worker's static prompt text.
//!
//! Prompt prose is never asserted in tests (it is not a machine-consumed
//! contract); the action protocol it documents is exercised through loop
//! behavior.

use crate::task::WorkerTask;

/// Static system prompt for the bootstrap worker. Never asserted in tests
/// (prompt prose is not a machine-consumed contract); the protocol it
/// documents is exercised through the loop's behavior.
/// The tool-listing header + the four mediated tools (shared by both
/// prompt variants — the `think` line composes in only when enabled, so
/// the prompt can never advertise a tool the executor refuses).
const PROMPT_HEAD: &str = "You are Orchestraitor's bootstrap worker operating on a task worktree.\n\
Every response MUST contain exactly one fenced ```json block selecting one of the ";

/// The `think` tool line + its usage guidance (§9.48). Composed into the
/// prompt ONLY when the kill-switch did not remove the tool.
const PROMPT_THINK: &str = "five tools:\n\
\n\
{\"tool\": \"read_file\", \"path\": \"<worktree-relative path>\"}\n\
{\"tool\": \"write_file\", \"path\": \"<worktree-relative path>\", \"content\": \"<full file content>\"}\n\
{\"tool\": \"search\", \"pattern\": \"<plain text>\", \"path\": \"<optional worktree-relative dir>\"}\n\
{\"tool\": \"bash\", \"script\": \"<bash script>\"}\n\
{\"tool\": \"think\", \"thought\": \"<this step's deliberation>\", \"thought_number\": <n>, \"total_thoughts\": <n>, \"next_thought_needed\": <bool>}\n\
\n\
Use `think` before complex work: estimate the step count, reason one step at\n\
a time, revise explicitly (set \"is_revision\": true with \"revises_thought\": <n>),\n\
and stop calling it when done. `think` performs no action — it only records\n\
your deliberation.\n";

/// The four-tool variant: identical surface minus the `think` line.
const PROMPT_NO_THINK: &str = "four tools:\n\
\n\
{\"tool\": \"read_file\", \"path\": \"<worktree-relative path>\"}\n\
{\"tool\": \"write_file\", \"path\": \"<worktree-relative path>\", \"content\": \"<full file content>\"}\n\
{\"tool\": \"search\", \"pattern\": \"<plain text>\", \"path\": \"<optional worktree-relative dir>\"}\n\
{\"tool\": \"bash\", \"script\": \"<bash script>\"}\n\
\n";

/// The terminator + refusal contract (shared by both variants).
const PROMPT_TAIL: &str = "Terminate with {\"tool\": \"finish\", \"summary\": \"<what was done>\", \"success\": true}\n\
or success=false when the task cannot be completed. Any tool not listed\n\
above (and not listed as a declared tool) is refused and recorded.";

/// Builds the system prompt: the static bootstrap prompt plus the one
/// sentence listing the run's role-visible declared tools (issue #535, T2).
/// Declared-tool lines are static, config-derived text (trusted layers by
/// the registry's gate); unknown tool names still produce typed refusals at
/// dispatch.
pub(super) fn system_prompt(config: &crate::result::WorkerConfig) -> String {
    // Inside a sub-session the tool surface is the ALLOWLIST (CR finding #2,
    // CWE-863): the prompt must never advertise a tool the executor would
    // refuse — a sub-session prompt is generated from the carved allowlist,
    // never from the bootstrap four.
    if config.subsession_depth > 0 {
        let mut prompt = String::from(
            "You are an Orchestraitor sub-session worker operating on a task worktree.\n\
             Every response MUST contain exactly one fenced ```json block selecting one of the allowed tools:\n\n",
        );
        let allowlist = &config.subsession_allowed_internal;
        for tool in [
            crate::tooldef::InternalTool::ReadFile,
            crate::tooldef::InternalTool::Search,
            crate::tooldef::InternalTool::Bash,
            crate::tooldef::InternalTool::Think,
        ] {
            if allowlist.contains(&tool)
                && (config.think_enabled || !matches!(tool, crate::tooldef::InternalTool::Think))
            {
                prompt.push_str(subsession_tool_line(tool));
                prompt.push('\n');
            }
        }
        prompt.push_str(
            "\nTerminate with {\"tool\": \"finish\", \"summary\": \"<what was done>\", \"success\": true}\n\
             or success=false when the task cannot be completed. Any tool not listed\n\
             above is refused and recorded.",
        );
        if config.structured_summary {
            prompt.push_str(
                "\n\nThe summary MUST be a compact fielded payload (machine-readable\n\
                 key: value lines, one fact per line) — never prose narrative.",
            );
        }
        return prompt;
    }
    // The §9.48.2 trusted-layer kill-switch composes the prompt from
    // constant pieces: the `think` line appears ONLY when the tool is on
    // the surface, so the prompt can never advertise a refused tool and no
    // exact-string surgery can silently fail.
    let mut prompt = format!(
        "{PROMPT_HEAD}{}{PROMPT_TAIL}",
        if config.think_enabled {
            PROMPT_THINK
        } else {
            PROMPT_NO_THINK
        }
    );
    if config.tools.is_empty() {
        return prompt;
    }
    prompt.push_str("\n\nDeclared tools available to you (invoked like any other tool):\n");
    for tool in &config.tools {
        prompt.push_str(&tool.prompt_line());
        prompt.push('\n');
    }
    prompt
}

/// The model-facing one-line usage for one allowlisted internal tool in a
/// sub-session prompt. Static text per tool; no config interpolation.
fn subsession_tool_line(tool: crate::tooldef::InternalTool) -> &'static str {
    match tool {
        crate::tooldef::InternalTool::ReadFile => {
            "{\"tool\": \"read_file\", \"path\": \"<worktree-relative path>\"}"
        }
        crate::tooldef::InternalTool::Search => {
            "{\"tool\": \"search\", \"pattern\": \"<plain text>\", \"path\": \"<optional worktree-relative dir>\"}"
        }
        crate::tooldef::InternalTool::Bash => "{\"tool\": \"bash\", \"script\": \"<bash script>\"}",
        crate::tooldef::InternalTool::Think => {
            "{\"tool\": \"think\", \"thought\": \"<this step's deliberation>\", \"thought_number\": <n ≥ 1>, \"total_thoughts\": <n ≥ 1>, \"next_thought_needed\": <bool>, \"is_revision\": <bool>, \"revises_thought\": <optional n>}"
        }
    }
}

/// The failed attempt's receipt log rides beside the static failure-reason
/// line as a fenced untrusted-data block (issue #657): `receipt_summary`
/// carries the prior attempt's bounded "what was already done" payload, or
/// `None` when the attempt recorded nothing worth summarizing.
pub(super) fn task_prompt(
    task: &WorkerTask,
    replan_note: Option<&'static str>,
    receipt_summary: Option<&str>,
) -> String {
    let mut prompt = format!("Task {} ({}):\n{}", task.id, task.slug, task.description);
    if let Some(note) = replan_note {
        prompt.push_str("\n\nA previous attempt failed (");
        prompt.push_str(note);
        prompt.push_str("); re-plan and try a different approach.");
    }
    if let Some(summary) = receipt_summary {
        prompt.push_str(summary);
    }
    prompt
}
