use std::io::IoSlice;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use anyhow::Result;
use librqbit::{
    ManagedTorrentShared, TorrentMetadata,
    storage::{
        BoxStorageFactory, StorageFactory, StorageFactoryExt, TorrentStorage,
        filesystem::FilesystemStorageFactory,
    },
};
use librqbit_core::lengths::ValidPieceIndex;

#[derive(Clone, Default)]
pub struct StorageCounters {
    pub creates: Arc<AtomicU64>,
    pub inits: Arc<AtomicU64>,
    pub reads: Arc<AtomicU64>,
    pub writes: Arc<AtomicU64>,
    pub completed_pieces: Arc<AtomicU64>,
    pub takes: Arc<AtomicU64>,
}

impl StorageCounters {
    pub fn snapshot(&self) -> serde_json::Value {
        serde_json::json!({
            "creates": self.creates.load(Ordering::Relaxed),
            "inits": self.inits.load(Ordering::Relaxed),
            "reads": self.reads.load(Ordering::Relaxed),
            "writes": self.writes.load(Ordering::Relaxed),
            "completed_pieces": self.completed_pieces.load(Ordering::Relaxed),
            "takes": self.takes.load(Ordering::Relaxed),
        })
    }
}

#[derive(Clone, Default)]
pub struct RecordingStorageFactory {
    pub counters: StorageCounters,
}

pub struct RecordingStorage {
    inner: Box<dyn TorrentStorage>,
    counters: StorageCounters,
}

impl StorageFactory for RecordingStorageFactory {
    type Storage = RecordingStorage;

    fn create(
        &self,
        shared: &ManagedTorrentShared,
        metadata: &TorrentMetadata,
    ) -> Result<Self::Storage> {
        self.counters.creates.fetch_add(1, Ordering::Relaxed);
        let inner = Box::new(FilesystemStorageFactory::default().create(shared, metadata)?)
            as Box<dyn TorrentStorage>;
        Ok(RecordingStorage {
            inner,
            counters: self.counters.clone(),
        })
    }

    fn clone_box(&self) -> BoxStorageFactory {
        self.clone().boxed()
    }
}

impl TorrentStorage for RecordingStorage {
    fn init(&mut self, shared: &ManagedTorrentShared, metadata: &TorrentMetadata) -> Result<()> {
        self.counters.inits.fetch_add(1, Ordering::Relaxed);
        self.inner.init(shared, metadata)
    }

    fn pread_exact(&self, file_id: usize, offset: u64, buf: &mut [u8]) -> Result<()> {
        self.counters.reads.fetch_add(1, Ordering::Relaxed);
        self.inner.pread_exact(file_id, offset, buf)
    }

    fn pwrite_all(&self, file_id: usize, offset: u64, buf: &[u8]) -> Result<()> {
        self.counters.writes.fetch_add(1, Ordering::Relaxed);
        self.inner.pwrite_all(file_id, offset, buf)
    }

    fn pwrite_all_vectored(
        &self,
        file_id: usize,
        offset: u64,
        bufs: [IoSlice<'_>; 2],
    ) -> Result<usize> {
        self.counters.writes.fetch_add(1, Ordering::Relaxed);
        self.inner.pwrite_all_vectored(file_id, offset, bufs)
    }

    fn remove_file(&self, file_id: usize, filename: &std::path::Path) -> Result<()> {
        self.inner.remove_file(file_id, filename)
    }

    fn remove_directory_if_empty(&self, path: &std::path::Path) -> Result<()> {
        self.inner.remove_directory_if_empty(path)
    }

    fn ensure_file_length(&self, file_id: usize, length: u64) -> Result<()> {
        self.inner.ensure_file_length(file_id, length)
    }

    fn take(&self) -> Result<Box<dyn TorrentStorage>> {
        self.counters.takes.fetch_add(1, Ordering::Relaxed);
        Ok(Box::new(Self {
            inner: self.inner.take()?,
            counters: self.counters.clone(),
        }))
    }

    fn on_piece_completed(&self, piece_index: ValidPieceIndex) -> Result<()> {
        self.counters
            .completed_pieces
            .fetch_add(1, Ordering::Relaxed);
        self.inner.on_piece_completed(piece_index)
    }
}
