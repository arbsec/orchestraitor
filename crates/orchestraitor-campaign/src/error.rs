//! Error types for the campaign lane. Messages are flat and path-scoped;
//! they never embed board content, task payloads, or credentials.

/// Errors surfaced by the campaign pass and its decision store.
#[derive(Debug, thiserror::Error)]
pub enum CampaignError {
    /// The decision store could not be opened, migrated, written, or read.
    #[error("campaign decision store at {path}: {source}")]
    Store {
        /// Store file path (or `:memory:` for in-memory stores).
        path: String,
        /// Underlying `SQLite` error.
        source: rusqlite::Error,
    },

    /// The selected task could not be loaded or run by the spawner. The
    /// spawner's message is expected to be already log-safe (the CLI path
    /// formats `miette` diagnostics; tests provide static text).
    #[error("worker spawn for task {task_id} failed: {message}")]
    Spawn {
        /// Task id that was selected.
        task_id: String,
        /// Spawner-provided, log-safe failure summary.
        message: String,
    },

    /// The worker role could not be resolved for the pass.
    #[error("worker role resolution failed: {0}")]
    Role(String),
}
