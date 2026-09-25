//! Persistent Rustorr state in SQLite: torrent catalog, settings and viewed
//! history. The single source of truth after a restart.
//!
//! The schema is versioned in `PRAGMA user_version` and only ever grows by
//! appending a migration. Nothing here knows about the BitTorrent engine or
//! the HTTP layer: torrent bytes and settings are stored, not interpreted.

mod catalog;
mod error;
mod schema;
mod settings;
mod snapshot;
mod state;
mod values;
mod viewed;
mod waf;

pub use catalog::CatalogEntry;
pub use error::Error;
pub use snapshot::{SCHEMA_VERSION, inspect, snapshot};
pub use state::State;
pub use viewed::ViewedEntry;
pub use waf::WafLists;
