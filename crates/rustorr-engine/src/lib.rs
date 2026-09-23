//! BitTorrent engine port and its `librqbit` adapter (ADR 0003).
//!
//! This is the only crate allowed to depend on `librqbit`. Its public API is
//! expressed in Rustorr terms; engine types and engine errors never cross
//! this boundary.

mod adapter;
mod cache_storage;
mod config;
mod error;
mod port;
#[cfg(test)]
mod session_tests;

pub use adapter::LibrqbitEngine;
pub use cache_storage::{CacheStorage, CacheStorageFactory};
pub use config::EngineConfig;
pub use error::Error;
pub use port::{
    AddOptions, DeletedTorrent, Engine, EngineFuture, EngineStatus, TorrentFile, TorrentMetadata,
    TorrentReader, TorrentSource, TorrentStatus,
};
