use std::{future::Future, io, net::SocketAddr, pin::Pin};

use rustorr_domain::{FileIndex, InfoHash};
use tokio::io::AsyncRead;

use crate::Error;

/// What a running engine reports about itself, read back after start rather
/// than copied from the configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineStatus {
    pub dht_enabled: bool,
    /// Port the engine accepts incoming peers on, if it listens at all.
    pub listen_port: Option<u16>,
}

/// Input accepted by the engine without leaking librqbit's `AddTorrent`.
#[derive(Debug, Clone)]
pub enum TorrentSource {
    TorrentBytes(Vec<u8>),
    Magnet(String),
    Url(String),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AddOptions {
    pub only_files: Option<Vec<usize>>,
    pub initial_peers: Vec<SocketAddr>,
}

/// Metadata Rustorr needs to persist and stream a resolved torrent.
#[derive(Debug, Clone)]
pub struct TorrentMetadata {
    pub hash: InfoHash,
    pub metainfo: Vec<u8>,
    pub file_lengths: Vec<u64>,
}

/// Runtime facts used by the narrow R5 readiness response. R6 will map the
/// full TorrServer status model instead of extending this probe structure.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TorrentStatus {
    pub ready: bool,
    pub live_peers: usize,
}

/// Facts captured immediately before an engine torrent is removed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeletedTorrent {
    pub live_peers: Vec<SocketAddr>,
}

/// A reader held by the engine. Its concrete librqbit implementation remains
/// private to this crate.
pub struct TorrentReader {
    inner: Pin<Box<dyn AsyncRead + Send>>,
    file_length: u64,
}

impl TorrentReader {
    pub fn new(reader: impl AsyncRead + Send + 'static, file_length: u64) -> Self {
        Self {
            inner: Box::pin(reader),
            file_length,
        }
    }

    pub fn file_length(&self) -> u64 {
        self.file_length
    }
}

impl AsyncRead for TorrentReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
        buffer: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        self.inner.as_mut().poll_read(context, buffer)
    }
}

pub type EngineFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, Error>> + Send + 'a>>;

/// The BitTorrent engine as the rest of Rustorr sees it, in domain terms.
///
/// Boxed futures keep the port object-safe without exposing an async-trait
/// macro or adapter implementation details to lifecycle policy.
pub trait Engine: Send + Sync {
    fn status(&self) -> &EngineStatus;
    fn is_loaded(&self, hash: InfoHash) -> bool;
    fn source_hash(&self, source: &TorrentSource) -> Option<InfoHash>;
    fn add(&self, source: TorrentSource, options: AddOptions) -> EngineFuture<'_, TorrentMetadata>;
    fn reader(
        &self,
        hash: InfoHash,
        file: FileIndex,
        offset: u64,
    ) -> EngineFuture<'_, TorrentReader>;
    fn piece_length(&self, hash: InfoHash) -> Result<u64, Error>;
    fn torrent_status(&self, hash: InfoHash) -> EngineFuture<'_, TorrentStatus>;
    fn delete(&self, hash: InfoHash) -> EngineFuture<'_, DeletedTorrent>;
}
