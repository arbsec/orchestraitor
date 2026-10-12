//! Integration tests for process supervision (spec §17.2.9 invariants 2 & 3).

use std::path::PathBuf;
use std::time::Duration;

use orchestraitor_daemon::{
    CommandSpec, ProcessStatus, SupervisionError, SupervisionEvent, Supervisor, WorkerSpec,
};
use orchestraitor_mcp::ProjectId;
use orchestraitor_model::SessionId;
use tokio::time::timeout;

/// Builds a command that exits immediately with a non-zero code (simulates a crash).
fn crash_command() -> CommandSpec {
    CommandSpec {
        program: PathBuf::from("false"),
        args: vec![],
        env: vec![],
    }
}

/// Builds a long-running `sleep` command that stays alive until killed.
fn sleep_command(seconds: u32) -> CommandSpec {
    CommandSpec {
        program: PathBuf::from("sleep"),
        args: vec![seconds.to_string()],
        env: vec![],
    }
}

/// Verifies §17.2.9 invariant 2: one worker failure MUST NOT terminate
/// unrelated work. A crashing worker's sibling must continue running.
#[tokio::test(flavor = "current_thread")]
async fn worker_crash_does_not_terminate_siblings() -> Result<(), Box<dyn std::error::Error>> {
    // Given: a supervisor with two workers — one that crashes and one that runs.
    let (mut supervisor, mut event_rx) = Supervisor::new();
    let project = ProjectId::new("test-project");
    let session_a = SessionId::new();
    let session_b = SessionId::new();

    supervisor.spawn_worker(WorkerSpec {
        session_id: session_a.clone(),
        project: project.clone(),
        command: crash_command(),
        max_restarts: 0,
    })?;
    supervisor.spawn_worker(WorkerSpec {
        session_id: session_b.clone(),
        project: project.clone(),
        command: sleep_command(60),
        max_restarts: 0,
    })?;

    // When: worker A crashes. Collect events until we see WorkerExited for A.
    loop {
        let event = timeout(Duration::from_secs(5), event_rx.recv())
            .await
            .map_err(|_| "timed out waiting for worker exit event")?
            .ok_or("event channel closed unexpectedly")?;
        if let SupervisionEvent::WorkerExited { session, .. } = &event
            && *session == session_a
        {
            break; // Worker A exited — sibling isolation verified below.
        }
    }

    // Then: worker B is still running (§17.2.9 invariant 2).
    let status_b = supervisor.worker_status(&session_b);
    assert!(
        matches!(status_b, Some(ProcessStatus::Running { .. })),
        "worker B must still be running after worker A crashed, got {status_b:?}"
    );

    // Cleanup.
    supervisor.shutdown_all();
    Ok(())
}

/// Verifies §17.2.9 invariant 3: one project MUST NEVER see another project's
/// tools. A worker bound to project A cannot access project B's tools.
#[tokio::test(flavor = "current_thread")]
async fn project_a_cannot_see_project_b_tools() -> Result<(), Box<dyn std::error::Error>> {
    // Given: a supervisor with tools registered for two isolated projects.
    let (mut supervisor, _event_rx) = Supervisor::new();
    let project_a = ProjectId::new("project-a");
    let project_b = ProjectId::new("project-b");

    supervisor.register_project_tools(
        &project_a,
        vec!["fs.read".to_string(), "fs.write".to_string()],
    );
    supervisor.register_project_tools(&project_b, vec!["fs.read".to_string()]);

    let session_a = SessionId::new();
    supervisor.spawn_worker(WorkerSpec {
        session_id: session_a.clone(),
        project: project_a.clone(),
        command: sleep_command(60),
        max_restarts: 0,
    })?;

    // When/Then: worker in project A can access its own project's tools.
    assert!(
        supervisor
            .check_tool_access(&session_a, "fs.read", &project_a)
            .is_ok(),
        "worker A should access its own project's registered tool"
    );
    assert!(
        supervisor
            .check_tool_access(&session_a, "fs.write", &project_a)
            .is_ok(),
        "worker A should access its own project's registered tool"
    );

    // Then: worker in project A CANNOT access project B's tools (§17.2.9 invariant 3).
    let result = supervisor.check_tool_access(&session_a, "fs.read", &project_b);
    assert!(
        matches!(result, Err(SupervisionError::CrossProjectAccess { .. })),
        "worker A must not access project B's tools, got {result:?}"
    );

    // Then: unregistered tool is refused even within the bound project.
    let result = supervisor.check_tool_access(&session_a, "fs.exec", &project_a);
    assert!(
        matches!(result, Err(SupervisionError::ToolNotRegistered { .. })),
        "unregistered tool must be refused, got {result:?}"
    );

    // Cleanup.
    supervisor.shutdown_all();
    Ok(())
}
