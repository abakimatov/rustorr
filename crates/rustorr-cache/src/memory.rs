use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock},
};

use rustorr_domain::{FileIndex, InfoHash};

use crate::{Error, PieceStore, Recovered, TorrentLayout, extents::Extents};

/// Matches the BitTorrent chunk size, so a chunk-aligned write fills a block.
const BLOCK_SIZE: usize = 16 * 1024;

/// RAM-only store. Memory is allocated per block as bytes arrive, so a large
/// torrent costs only what has been written, plus at most one partial block
/// per stored span.
#[derive(Default)]
pub struct MemoryStore {
    torrents: RwLock<HashMap<InfoHash, Arc<Mutex<Torrent>>>>,
}

struct Torrent {
    layout: TorrentLayout,
    files: Vec<File>,
}

#[derive(Default)]
struct File {
    extents: Extents,
    blocks: HashMap<u64, Box<[u8]>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn torrent(&self, torrent: InfoHash) -> Result<Arc<Mutex<Torrent>>, Error> {
        self.torrents
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&torrent)
            .cloned()
            .ok_or(Error::UnknownTorrent(torrent))
    }
}

fn lock(torrent: &Mutex<Torrent>) -> MutexGuard<'_, Torrent> {
    torrent.lock().unwrap_or_else(PoisonError::into_inner)
}

impl PieceStore for MemoryStore {
    fn open(&self, torrent: InfoHash, layout: &TorrentLayout) -> Result<Recovered, Error> {
        let files = (0..layout.file_count()).map(|_| File::default()).collect();
        let created = Arc::new(Mutex::new(Torrent {
            layout: layout.clone(),
            files,
        }));
        self.torrents
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(torrent, created);
        Ok(Recovered::default())
    }

    fn write(
        &self,
        torrent: InfoHash,
        file: FileIndex,
        offset: u64,
        data: &[u8],
    ) -> Result<u64, Error> {
        let handle = self.torrent(torrent)?;
        let mut guard = lock(&handle);
        let range = guard
            .layout
            .check_range(torrent, file, offset, data.len())?;
        let stored = &mut guard.files[file.zero_based() as usize];

        let mut position = offset;
        let mut rest = data;
        while !rest.is_empty() {
            let within = (position % BLOCK_SIZE as u64) as usize;
            let count = rest.len().min(BLOCK_SIZE - within);
            let block = stored
                .blocks
                .entry(position / BLOCK_SIZE as u64)
                .or_insert_with(|| vec![0; BLOCK_SIZE].into_boxed_slice());
            block[within..within + count].copy_from_slice(&rest[..count]);
            position += count as u64;
            rest = &rest[count..];
        }
        Ok(stored.extents.insert(range))
    }

    fn read(
        &self,
        torrent: InfoHash,
        file: FileIndex,
        offset: u64,
        buf: &mut [u8],
    ) -> Result<(), Error> {
        let handle = self.torrent(torrent)?;
        let guard = lock(&handle);
        let range = guard.layout.check_range(torrent, file, offset, buf.len())?;
        let stored = &guard.files[file.zero_based() as usize];
        let missing = || Error::Missing {
            torrent,
            file,
            range,
        };
        if !stored.extents.covers(range) {
            return Err(missing());
        }

        let mut position = offset;
        let mut filled = 0;
        while filled < buf.len() {
            let within = (position % BLOCK_SIZE as u64) as usize;
            let count = (buf.len() - filled).min(BLOCK_SIZE - within);
            let block = stored
                .blocks
                .get(&(position / BLOCK_SIZE as u64))
                .ok_or_else(missing)?;
            buf[filled..filled + count].copy_from_slice(&block[within..within + count]);
            position += count as u64;
            filled += count;
        }
        Ok(())
    }

    fn remove(&self, torrent: InfoHash) -> Result<(), Error> {
        self.torrents
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&torrent);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    crate::testing::store_contract_tests!((MemoryStore::new(), ()));
}
