//! SQL schema text for daemon-store migrations.

/// Initial daemon-store schema.
pub(crate) const SCHEMA_V1: &str = "
CREATE TABLE IF NOT EXISTS session_metadata (
  session_id TEXT PRIMARY KEY, repository_id TEXT NOT NULL, adapter_id TEXT NOT NULL, workspace_id TEXT NOT NULL, security_mode TEXT NOT NULL, policy_digest TEXT NOT NULL, state TEXT NOT NULL, created_at TEXT NOT NULL, payload_json TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS cost_ledger (
  request_id TEXT PRIMARY KEY, session_id TEXT NOT NULL, provider_id TEXT NOT NULL, model_id TEXT NOT NULL, payload_json TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS event_records (
  monotonic_seq INTEGER PRIMARY KEY, schema_version INTEGER NOT NULL, category TEXT NOT NULL, correlation_id TEXT NOT NULL, parent_op_id TEXT, prev_hash TEXT, record_hash TEXT NOT NULL, record_json TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS backlog_state (
  item_id TEXT PRIMARY KEY, session_id TEXT, state TEXT NOT NULL, payload_json TEXT NOT NULL, updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS arbitraitor_receipts (
  receipt_id TEXT PRIMARY KEY, session_id TEXT, receipt_kind TEXT NOT NULL, digest TEXT, payload_json TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS delegation_chains (
  chain_id TEXT PRIMARY KEY, session_id TEXT, parent_op_id TEXT NOT NULL, child_op_id TEXT NOT NULL, payload_json TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_event_records_category ON event_records(category);
CREATE INDEX IF NOT EXISTS idx_event_records_correlation ON event_records(correlation_id);
CREATE INDEX IF NOT EXISTS idx_cost_ledger_session ON cost_ledger(session_id);
CREATE INDEX IF NOT EXISTS idx_backlog_state_session ON backlog_state(session_id);
CREATE INDEX IF NOT EXISTS idx_receipts_session ON arbitraitor_receipts(session_id);
CREATE INDEX IF NOT EXISTS idx_delegation_session ON delegation_chains(session_id);
";
