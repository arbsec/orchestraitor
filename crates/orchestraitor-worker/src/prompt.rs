//! The bootstrap worker's static prompt text.
//!
//! Prompt prose is never asserted in tests (it is not a machine-consumed
//! contract); the action protocol it documents is exercised through loop
//! behavior.

use crate::task::WorkerTask;

/// Static system prompt for the bootstrap worker. Never asserted in tests
/// (prompt prose is not a machine-consumed contract); the protocol it
/// documents is exercised through the loop's behavior.
pub(super) const SYSTEM_PROMPT: &str = "You are Orchestraitor's bootstrap worker operating on a task worktree.
Every response MUST contain exactly one fenced ```json block selecting one of the four tools:

{\"tool\": \"read_file\", \"path\": \"<worktree-relative path>\"}
{\"tool\": \"write_file\", \"path\": \"<worktree-relative path>\", \"content\": \"<full file content>\"}
{\"tool\": \"search\", \"pattern\": \"<plain text>\", \"path\": \"<optional worktree-relative dir>\"}
{\"tool\": \"bash\", \"script\": \"<bash script>\"}

Terminate with {\"tool\": \"finish\", \"summary\": \"<what was done>\", \"success\": true}
or success=false when the task cannot be completed. No other tools exist:
any other capability is refused and recorded.";

pub(super) fn task_prompt(task: &WorkerTask, replan_note: Option<&'static str>) -> String {
    let mut prompt = format!("Task {} ({}):\n{}", task.id, task.slug, task.description);
    if let Some(note) = replan_note {
        prompt.push_str("\n\nA previous attempt failed (");
        prompt.push_str(note);
        prompt.push_str("); re-plan and try a different approach.");
    }
    prompt
}
