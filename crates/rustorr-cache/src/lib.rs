//! Rustorr-owned piece cache (ADR 0004): residency index, torrent-scoped LRU
//! with pinned views, and the `PieceStore` seam. Knows nothing about the
//! BitTorrent engine.
//!
//! Storage backends: [`MemoryStore`] for RAM-only mode and [`DiskStore`] for
//! SSD. On Linux the page cache already keeps recently used disk data in RAM,
//! so an explicit two-tier RAM+SSD store waits for R5 measurements.

mod cache;
mod disk;
mod error;
mod extents;
mod layout;
mod memory;
mod store;
#[cfg(test)]
mod testing;

pub use cache::{Cache, CacheConfig, CacheStats, Pin};
pub use disk::DiskStore;
pub use error::Error;
pub use layout::TorrentLayout;
pub use memory::MemoryStore;
pub use store::PieceStore;
