//! SQLite-backed cost-ledger storage.

use crate::budget::{BudgetScope, CapKind, CapMetric, StoredCap};
use crate::error::{LedgerError, LedgerResult};
use crate::model::{
    CostEntry, DomainCostRollup, MonetaryCostBasis, Subscription, SubscriptionId,
    SubscriptionUtilizationEntry, UtilizationLabel,
};
use orchestraitor_model::AgentId;
use rusqlite::{Connection, OptionalExtension, Row, params};
use std::path::Path;

/// SQLite-backed cost ledger.
pub struct CostLedger {
    conn: Connection,
}

impl CostLedger {
    /// Opens a ledger database and creates the schema when needed.
    ///
    /// # Errors
    /// Returns [`LedgerError`] when `SQLite` cannot open or initialize the database.
    pub fn open(path: &Path) -> LedgerResult<Self> {
        let ledger = Self {
            conn: Connection::open(path)?,
        };
        ledger.init_schema()?;
        Ok(ledger)
    }

    /// Creates an in-memory ledger for tests and short-lived callers.
    ///
    /// # Errors
    /// Returns [`LedgerError`] when `SQLite` cannot initialize the in-memory database.
    pub fn open_in_memory() -> LedgerResult<Self> {
        let ledger = Self {
            conn: Connection::open_in_memory()?,
        };
        ledger.init_schema()?;
        Ok(ledger)
    }

    /// Returns the separate API spend table facade.
    #[must_use]
    pub const fn api_spend(&self) -> ApiSpendTable<'_> {
        ApiSpendTable { conn: &self.conn }
    }

    /// Returns the separate subscription utilization table facade.
    #[must_use]
    pub const fn subscription_utilization(&self) -> SubscriptionUtilizationTable<'_> {
        SubscriptionUtilizationTable { conn: &self.conn }
    }

    /// Inserts optional subscription metadata from spec §9.19.5.
    ///
    /// # Errors
    /// Returns [`LedgerError`] when `SQLite` insertion fails or integer conversion overflows.
    pub fn insert_subscription(&self, subscription: &Subscription) -> LedgerResult<()> {
        self.conn.execute(
            "INSERT INTO subscriptions (id, provider, billing_period, monthly_price_usd, included_tokens, soft_cap_tokens, hard_cap_tokens, active_time_cap_minutes_per_day, reset_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![subscription.id.as_str(), subscription.provider.as_str(), subscription.billing_period, subscription.monthly_price_usd, opt_i64(subscription.included_tokens)?, opt_i64(subscription.soft_cap_tokens)?, opt_i64(subscription.hard_cap_tokens)?, opt_i64(subscription.active_time_cap_minutes_per_day)?, subscription.reset_at],
        )?;
        Ok(())
    }

    /// Inserts a budget row and returns its `SQLite` id.
    ///
    /// # Errors
    /// Returns [`LedgerError`] when `SQLite` insertion fails.
    pub fn insert_budget(&self, scope: BudgetScope, scope_id: &str) -> LedgerResult<i64> {
        self.conn.execute(
            "INSERT INTO budgets (scope, scope_id) VALUES (?1, ?2)",
            params![scope.as_str(), scope_id],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Inserts a soft or hard cap for a budget.
    ///
    /// # Errors
    /// Returns [`LedgerError`] when `SQLite` insertion fails.
    pub fn insert_cap(
        &self,
        budget_id: i64,
        metric: CapMetric,
        kind: CapKind,
        amount: f64,
    ) -> LedgerResult<()> {
        self.conn.execute(
            "INSERT INTO caps (budget_id, metric, kind, amount) VALUES (?1, ?2, ?3, ?4)",
            params![budget_id, metric.as_str(), kind.as_str(), amount],
        )?;
        Ok(())
    }

    pub(crate) fn token_usage_for_scope(
        &self,
        scope: BudgetScope,
        scope_id: &str,
    ) -> LedgerResult<u64> {
        let (where_sql, value) = scope_filter(scope, scope_id);
        let mut stmt = self.conn.prepare(&format!("SELECT input_tokens + output_tokens + reasoning_tokens + cache_read_tokens + cache_write_tokens FROM cost_entries WHERE {where_sql}"))?;
        let rows = stmt.query_map(params![value], |row| row.get::<_, i64>(0))?;
        let mut total = 0_u64;
        for row in rows {
            total = total.saturating_add(u64_from_i64(row?)?);
        }
        Ok(total)
    }

    pub(crate) fn cost_usage_for_scope(
        &self,
        scope: BudgetScope,
        scope_id: &str,
    ) -> LedgerResult<f64> {
        let (where_sql, value) = scope_filter(scope, scope_id);
        self.conn.query_row(
            &format!("SELECT COALESCE(SUM(COALESCE(monetary_cost_measured, monetary_cost_estimated, 0)), 0) FROM cost_entries WHERE {where_sql}"),
            params![value],
            |row| row.get(0),
        ).map_err(Into::into)
    }

    pub(crate) fn caps_for_scope(
        &self,
        scope: BudgetScope,
        scope_id: &str,
    ) -> LedgerResult<Vec<StoredCap>> {
        let mut stmt = self.conn.prepare("SELECT c.metric, c.kind, c.amount FROM caps c JOIN budgets b ON b.id = c.budget_id WHERE b.scope = ?1 AND b.scope_id = ?2")?;
        let rows = stmt.query_map(params![scope.as_str(), scope_id], cap_from_row)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Returns whether a `SQLite` table exists in this ledger database.
    ///
    /// # Errors
    /// Returns [`LedgerError`] when `SQLite` cannot inspect the schema.
    pub fn has_table(&self, table_name: &str) -> LedgerResult<bool> {
        let exists = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
            params![table_name],
            |row| row.get::<_, bool>(0),
        )?;
        Ok(exists)
    }

    fn init_schema(&self) -> LedgerResult<()> {
        self.conn.execute_batch(SCHEMA)?;
        Ok(())
    }
}

/// Separate table facade for metered API spend.
pub struct ApiSpendTable<'a> {
    conn: &'a Connection,
}

impl ApiSpendTable<'_> {
    /// Inserts one per-call cost entry into the API spend table.
    ///
    /// # Errors
    /// Returns [`LedgerError`] when `SQLite` insertion fails or integer conversion overflows.
    pub fn insert_cost_entry(&self, entry: &CostEntry) -> LedgerResult<()> {
        insert_entry(self.conn, entry)
    }

    /// Queries a per-domain rollup from API spend rows.
    ///
    /// # Errors
    /// Returns [`LedgerError`] when `SQLite` query or integer conversion fails.
    pub fn domain_rollup(
        &self,
        agent_domain_id: &AgentId,
    ) -> LedgerResult<Option<DomainCostRollup>> {
        self.conn
            .query_row(
                ROLLUP_SQL,
                params![agent_domain_id.as_str()],
                rollup_from_row,
            )
            .optional()
            .map_err(Into::into)
    }
}

/// Separate table facade for flat-rate subscription utilization.
pub struct SubscriptionUtilizationTable<'a> {
    conn: &'a Connection,
}

impl SubscriptionUtilizationTable<'_> {
    /// Inserts one utilization row without adding metered API spend.
    ///
    /// # Errors
    /// Returns [`LedgerError`] when `SQLite` insertion fails or integer conversion overflows.
    pub fn insert_utilization(&self, entry: &SubscriptionUtilizationEntry) -> LedgerResult<()> {
        self.conn.execute(
            "INSERT INTO subscription_utilization (subscription_id, request_id, label, consumed_tokens, quota_tokens, monthly_price_usd) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![entry.subscription_id.as_str(), entry.request_id, entry.label.as_str(), i64_from_u64(entry.consumed_tokens)?, opt_i64(entry.quota_tokens)?, entry.monthly_price_usd],
        )?;
        Ok(())
    }

    /// Returns the utilization label for a request id.
    ///
    /// # Errors
    /// Returns [`LedgerError`] when `SQLite` query fails or a stored label is invalid.
    pub fn label_for_request(&self, request_id: &str) -> LedgerResult<Option<UtilizationLabel>> {
        self.conn
            .query_row(
                "SELECT label FROM subscription_utilization WHERE request_id = ?1",
                params![request_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|value| parse_label(&value))
            .transpose()
    }
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS cost_entries (
  request_id TEXT PRIMARY KEY, model TEXT NOT NULL, provider TEXT NOT NULL, agent_domain_id TEXT NOT NULL, role TEXT NOT NULL, project TEXT NOT NULL, session TEXT NOT NULL, repository TEXT NOT NULL,
  input_tokens INTEGER NOT NULL, output_tokens INTEGER NOT NULL, reasoning_tokens INTEGER NOT NULL, cache_read_tokens INTEGER NOT NULL, cache_write_tokens INTEGER NOT NULL, request_count INTEGER NOT NULL,
  parent_request_id TEXT, started_at TEXT NOT NULL, completed_at TEXT NOT NULL, wall_ms INTEGER NOT NULL, monetary_cost_measured REAL, monetary_cost_estimated REAL, monetary_cost_basis TEXT NOT NULL,
  subscription_attribution_id TEXT, routing_decision TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS subscription_utilization (id INTEGER PRIMARY KEY, subscription_id TEXT NOT NULL, request_id TEXT NOT NULL, label TEXT NOT NULL, consumed_tokens INTEGER NOT NULL, quota_tokens INTEGER, monthly_price_usd REAL);
CREATE TABLE IF NOT EXISTS subscriptions (id TEXT PRIMARY KEY, provider TEXT NOT NULL, billing_period TEXT NOT NULL, monthly_price_usd REAL, included_tokens INTEGER, soft_cap_tokens INTEGER, hard_cap_tokens INTEGER, active_time_cap_minutes_per_day INTEGER, reset_at TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS budgets (id INTEGER PRIMARY KEY, scope TEXT NOT NULL, scope_id TEXT NOT NULL, currency TEXT, monthly_amount REAL, per_day_amount REAL, per_session_token_cap INTEGER, per_agent_token_cap INTEGER, per_session_cost_cap REAL);
CREATE TABLE IF NOT EXISTS caps (id INTEGER PRIMARY KEY, budget_id INTEGER NOT NULL REFERENCES budgets(id), metric TEXT NOT NULL, kind TEXT NOT NULL, amount REAL NOT NULL);
";

const ROLLUP_SQL: &str = "SELECT agent_domain_id, SUM(input_tokens), SUM(output_tokens), SUM(reasoning_tokens), SUM(cache_read_tokens), SUM(cache_write_tokens), SUM(request_count), COALESCE(SUM(monetary_cost_measured), 0), COALESCE(SUM(monetary_cost_estimated), 0) FROM cost_entries WHERE agent_domain_id = ?1 GROUP BY agent_domain_id";

fn insert_entry(conn: &Connection, entry: &CostEntry) -> LedgerResult<()> {
    conn.execute("INSERT INTO cost_entries (request_id, model, provider, agent_domain_id, role, project, session, repository, input_tokens, output_tokens, reasoning_tokens, cache_read_tokens, cache_write_tokens, request_count, parent_request_id, started_at, completed_at, wall_ms, monetary_cost_measured, monetary_cost_estimated, monetary_cost_basis, subscription_attribution_id, routing_decision) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23)", params![entry.request_id, entry.model.as_str(), entry.provider.as_str(), entry.agent_domain_id.as_str(), entry.role, entry.project, entry.session.as_str(), entry.repository.as_str(), i64_from_u64(entry.input_tokens)?, i64_from_u64(entry.output_tokens)?, i64_from_u64(entry.reasoning_tokens)?, i64_from_u64(entry.cache_read_tokens)?, i64_from_u64(entry.cache_write_tokens)?, i64_from_u64(entry.request_count)?, entry.parent_request_id, entry.started_at.to_rfc3339(), entry.completed_at.to_rfc3339(), i64_from_u64(entry.wall_ms)?, entry.monetary_cost_measured, entry.monetary_cost_estimated, basis_text(entry.monetary_cost_basis), entry.subscription_attribution_id.as_ref().map(SubscriptionId::as_str), entry.routing_decision])?;
    Ok(())
}

fn rollup_from_row(row: &Row<'_>) -> Result<DomainCostRollup, rusqlite::Error> {
    Ok(DomainCostRollup {
        agent_domain_id: AgentId::from_string(row.get(0)?),
        input_tokens: u64_from_sql(row, 1)?,
        output_tokens: u64_from_sql(row, 2)?,
        reasoning_tokens: u64_from_sql(row, 3)?,
        cache_read_tokens: u64_from_sql(row, 4)?,
        cache_write_tokens: u64_from_sql(row, 5)?,
        request_count: u64_from_sql(row, 6)?,
        monetary_cost_measured: row.get(7)?,
        monetary_cost_estimated: row.get(8)?,
    })
}

fn cap_from_row(row: &Row<'_>) -> Result<StoredCap, rusqlite::Error> {
    Ok(StoredCap {
        metric: parse_metric_sql(&row.get::<_, String>(0)?)?,
        kind: parse_kind_sql(&row.get::<_, String>(1)?)?,
        amount: row.get(2)?,
    })
}

fn u64_from_sql(row: &Row<'_>, index: usize) -> Result<u64, rusqlite::Error> {
    let value = row.get::<_, i64>(index)?;
    u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(index, value))
}

fn i64_from_u64(value: u64) -> LedgerResult<i64> {
    i64::try_from(value).map_err(|_| LedgerError::IntegerRange)
}

fn opt_i64(value: Option<u64>) -> LedgerResult<Option<i64>> {
    value.map(i64_from_u64).transpose()
}

fn u64_from_i64(value: i64) -> LedgerResult<u64> {
    u64::try_from(value).map_err(|_| LedgerError::IntegerRange)
}

fn basis_text(basis: MonetaryCostBasis) -> &'static str {
    match basis {
        MonetaryCostBasis::ProviderMeasured => "provider-measured",
        MonetaryCostBasis::PriceSheetEstimated => "price-sheet-estimated",
        MonetaryCostBasis::UserConfiguredSubscriptionPrice => "user-configured-subscription-price",
        MonetaryCostBasis::UtilizationOnly => "utilization-only",
    }
}

fn parse_label(value: &str) -> LedgerResult<UtilizationLabel> {
    match value {
        "measured" => Ok(UtilizationLabel::Measured),
        "estimated" => Ok(UtilizationLabel::Estimated),
        "user-configured" => Ok(UtilizationLabel::UserConfigured),
        other => Err(LedgerError::InvalidStoredValue(other.to_owned())),
    }
}

fn parse_metric_sql(value: &str) -> Result<CapMetric, rusqlite::Error> {
    match value {
        "tokens" => Ok(CapMetric::Tokens),
        "cost" => Ok(CapMetric::Cost),
        other => Err(rusqlite::Error::InvalidParameterName(other.to_owned())),
    }
}

fn parse_kind_sql(value: &str) -> Result<CapKind, rusqlite::Error> {
    match value {
        "soft" => Ok(CapKind::Soft),
        "hard" => Ok(CapKind::Hard),
        other => Err(rusqlite::Error::InvalidParameterName(other.to_owned())),
    }
}

fn scope_filter(scope: BudgetScope, scope_id: &str) -> (&'static str, &str) {
    match scope {
        BudgetScope::Project => ("project = ?1", scope_id),
        BudgetScope::Session => ("session = ?1", scope_id),
        BudgetScope::Domain | BudgetScope::Agent => ("agent_domain_id = ?1", scope_id),
    }
}
