//! Live-first `models.dev` metadata client with cached and bundled fallback.

#![forbid(unsafe_code)]

pub mod catalog;
pub mod client;
pub mod error;

pub use catalog::{CatalogSource, ModelsDevCatalog};
pub use client::ModelsDevClient;
pub use error::ModelsDevError;
