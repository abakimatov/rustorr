use rustorr_domain::{FileIndex, InfoHash};

use crate::{Error, TorrentLayout};

/// Backing storage for torrent bytes, addressed the way the engine addresses
/// them: torrent, file and byte offset.
///
/// A store remembers which byte ranges it has received and refuses to read any
/// other, so a range that was removed or never written surfaces as
/// [`Error::Missing`] instead of as zeros (ADR 0004, decision 5).
///
/// Implementations are shared between engine threads and must be thread-safe.
pub trait PieceStore: Send + Sync {
    /// Creates empty storage for a torrent, discarding anything held for it
    /// before. Deciding whether existing storage may be reused is the caller's
    /// job (see `Cache::open`).
    fn open(&self, torrent: InfoHash, layout: &TorrentLayout) -> Result<(), Error>;

    /// Stores `data` at `offset` and returns how many bytes were not stored
    /// before, so rewriting the same range adds nothing.
    fn write(
        &self,
        torrent: InfoHash,
        file: FileIndex,
        offset: u64,
        data: &[u8],
    ) -> Result<u64, Error>;

    /// Fills `buf` from `offset`. If any byte of the range was never stored it
    /// fails, and `buf` must not be used.
    fn read(
        &self,
        torrent: InfoHash,
        file: FileIndex,
        offset: u64,
        buf: &mut [u8],
    ) -> Result<(), Error>;

    /// Drops everything held for the torrent. Only valid once the engine no
    /// longer uses the torrent's storage.
    fn remove(&self, torrent: InfoHash) -> Result<(), Error>;
}
