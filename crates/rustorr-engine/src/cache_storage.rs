//! The seam from ADR 0004: librqbit's storage traits implemented over the
//! Rustorr cache, so every byte the engine reads or writes goes through it.

use std::{path::Path, sync::Arc};

use anyhow::{Context, bail};
use librqbit::{
    ManagedTorrentShared, TorrentMetadata,
    storage::{BoxStorageFactory, StorageFactory, StorageFactoryExt, TorrentStorage},
};
use librqbit_core::lengths::ValidPieceIndex;
use rustorr_cache::{Cache, TorrentLayout};
use rustorr_domain::{FileIndex, InfoHash, PieceIndex};

/// Hands the engine a [`CacheStorage`] for each torrent it starts.
#[derive(Clone)]
pub struct CacheStorageFactory {
    cache: Arc<Cache>,
}

impl CacheStorageFactory {
    pub fn new(cache: Arc<Cache>) -> Self {
        Self { cache }
    }
}

impl StorageFactory for CacheStorageFactory {
    type Storage = CacheStorage;

    /// The engine calls this again after every pause and resume; the cache
    /// re-attaches to what it already holds for the torrent.
    fn create(
        &self,
        shared: &ManagedTorrentShared,
        metadata: &TorrentMetadata,
    ) -> anyhow::Result<CacheStorage> {
        let torrent = InfoHash::from_bytes(shared.info_hash.0);
        let layout = TorrentLayout::new(
            u64::from(metadata.info.lengths().default_piece_length()),
            metadata.file_infos.iter().map(|file| file.len).collect(),
        )?;
        self.cache.open(torrent, layout)?;
        Ok(CacheStorage {
            cache: Arc::clone(&self.cache),
            torrent,
        })
    }

    fn clone_box(&self) -> BoxStorageFactory {
        self.clone().boxed()
    }
}

/// One torrent's view of the cache. A handle only, holding no data itself.
pub struct CacheStorage {
    cache: Arc<Cache>,
    torrent: InfoHash,
}

fn file_index(file_id: usize) -> anyhow::Result<FileIndex> {
    let index = u32::try_from(file_id).context("file id does not fit in 32 bits")?;
    Ok(FileIndex::from_zero_based(index))
}

impl TorrentStorage for CacheStorage {
    /// Nothing to do: the layout was registered in `create`.
    fn init(
        &mut self,
        _shared: &ManagedTorrentShared,
        _metadata: &TorrentMetadata,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    /// Fails for bytes the cache does not hold. The engine's initial check
    /// treats a failed read as "piece needed", so an empty cache is safe.
    fn pread_exact(&self, file_id: usize, offset: u64, buf: &mut [u8]) -> anyhow::Result<()> {
        Ok(self
            .cache
            .read(self.torrent, file_index(file_id)?, offset, buf)?)
    }

    fn pwrite_all(&self, file_id: usize, offset: u64, buf: &[u8]) -> anyhow::Result<()> {
        Ok(self
            .cache
            .write(self.torrent, file_index(file_id)?, offset, buf)?)
    }

    /// Deleting a torrent's data is the cache's decision, made after the
    /// engine has released the torrent, so the engine's own delete does not
    /// remove anything.
    fn remove_file(&self, _file_id: usize, _filename: &Path) -> anyhow::Result<()> {
        Ok(())
    }

    fn remove_directory_if_empty(&self, _path: &Path) -> anyhow::Result<()> {
        Ok(())
    }

    /// Files are not pre-sized; this only checks that the engine agrees with
    /// the layout the cache was opened with.
    fn ensure_file_length(&self, file_id: usize, length: u64) -> anyhow::Result<()> {
        let known = self.cache.file_length(self.torrent, file_index(file_id)?)?;
        if known != length {
            bail!("file {file_id} has {length} bytes in the engine but {known} in the cache");
        }
        Ok(())
    }

    fn take(&self) -> anyhow::Result<Box<dyn TorrentStorage>> {
        Ok(Box::new(Self {
            cache: Arc::clone(&self.cache),
            torrent: self.torrent,
        }))
    }

    fn on_piece_completed(&self, piece: ValidPieceIndex) -> anyhow::Result<()> {
        Ok(self
            .cache
            .piece_completed(self.torrent, PieceIndex::new(piece.get()))?)
    }
}

#[cfg(test)]
mod tests {
    use librqbit_core::lengths::Lengths;
    use rustorr_cache::{CacheConfig, MemoryStore};

    use super::*;

    const FILE_LENGTHS: [u64; 2] = [1000, 500];

    fn torrent() -> InfoHash {
        InfoHash::from_bytes([7; 20])
    }

    fn storage() -> CacheStorage {
        let cache = Arc::new(Cache::new(
            Arc::new(MemoryStore::new()),
            CacheConfig::default(),
        ));
        cache
            .open(
                torrent(),
                TorrentLayout::new(256, FILE_LENGTHS.to_vec()).unwrap(),
            )
            .unwrap();
        CacheStorage {
            cache,
            torrent: torrent(),
        }
    }

    #[test]
    fn what_the_engine_writes_it_can_read_back() {
        let storage = storage();
        storage.pwrite_all(0, 100, &[1, 2, 3, 4]).unwrap();
        storage.pwrite_all(1, 0, &[9; 8]).unwrap();

        let mut buf = [0; 4];
        storage.pread_exact(0, 100, &mut buf).unwrap();
        assert_eq!(buf, [1, 2, 3, 4]);
        let mut buf = [0; 8];
        storage.pread_exact(1, 0, &mut buf).unwrap();
        assert_eq!(buf, [9; 8]);
    }

    #[test]
    fn bytes_the_engine_never_wrote_are_an_error_not_zeros() {
        let storage = storage();
        let mut buf = [0xAA; 16];

        assert!(storage.pread_exact(0, 0, &mut buf).is_err());
        assert_eq!(buf, [0xAA; 16]);

        storage.pwrite_all(0, 0, &[1; 8]).unwrap();
        assert!(
            storage.pread_exact(0, 0, &mut buf).is_err(),
            "a half-written range must not read as data"
        );
    }

    #[test]
    fn a_torrent_removed_under_the_engine_fails_reads_instead_of_returning_zeros() {
        let storage = storage();
        storage.pwrite_all(0, 0, &[5; 16]).unwrap();
        storage.cache.remove(torrent()).unwrap();

        let mut buf = [0xAA; 16];
        assert!(storage.pread_exact(0, 0, &mut buf).is_err());
        assert_eq!(buf, [0xAA; 16]);
    }

    #[test]
    fn completed_pieces_are_recorded_in_the_residency_index() {
        let storage = storage();
        let lengths = Lengths::new(1500, 256).unwrap();
        let piece = lengths.validate_piece_index(4).unwrap();

        assert!(
            !storage
                .cache
                .is_piece_resident(torrent(), PieceIndex::new(4))
                .unwrap()
        );
        storage.on_piece_completed(piece).unwrap();
        assert!(
            storage
                .cache
                .is_piece_resident(torrent(), PieceIndex::new(4))
                .unwrap()
        );
    }

    #[test]
    fn file_lengths_must_agree_with_the_layout() {
        let storage = storage();

        storage.ensure_file_length(0, 1000).unwrap();
        storage.ensure_file_length(1, 500).unwrap();
        assert!(storage.ensure_file_length(1, 501).is_err());
        assert!(storage.ensure_file_length(2, 1).is_err());
    }

    #[test]
    fn the_engines_own_delete_leaves_the_data_to_the_cache() {
        let storage = storage();
        storage.pwrite_all(0, 0, &[3; 4]).unwrap();

        storage.remove_file(0, Path::new("a.mkv")).unwrap();
        storage.remove_directory_if_empty(Path::new("dir")).unwrap();

        let mut buf = [0; 4];
        storage.pread_exact(0, 0, &mut buf).unwrap();
        assert_eq!(buf, [3; 4]);
    }

    #[test]
    fn a_taken_handle_still_serves_the_same_torrent() {
        let storage = storage();
        storage.pwrite_all(0, 0, &[8; 4]).unwrap();

        let taken = storage.take().unwrap();

        let mut buf = [0; 4];
        taken.pread_exact(0, 0, &mut buf).unwrap();
        assert_eq!(buf, [8; 4]);
    }

    #[test]
    fn a_file_id_that_does_not_fit_is_an_error() {
        let storage = storage();
        let mut buf = [0; 1];

        assert!(storage.pread_exact(usize::MAX, 0, &mut buf).is_err());
    }
}
