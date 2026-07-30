//! SQLite WAL metadata store plus filesystem SHA-256 CAS.
//!
//! The store implements spec §9.17 and tech-stack §11 for the daemon: session
//! metadata, cost rows, event records, backlog state, Arbitraitor receipts,
//! delegation chains, migrations, and content-addressed blobs. It does not make
//! security decisions; Arbitraitor-originated receipts are persisted as data.

mod cas;
mod error;
mod records;
mod schema;
mod sqlite;

#[cfg(test)]
mod tests;

pub use cas::CasDirectory;
pub use error::{StoreError, StoreResult};
pub use records::{BacklogStateRecord, CostLedgerRecord, DelegationRecord, ReceiptRecord};
pub use sqlite::{DaemonStore, LATEST_SCHEMA_VERSION, StorePaths};
