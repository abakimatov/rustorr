//! Torrent lifecycle policy: the only module allowed to coordinate engine
//! sessions, Rustorr cache eviction and the persistent catalog.

use std::{
    collections::HashMap,
    io,
    pin::Pin,
    sync::{
        Arc, Weak,
        atomic::{AtomicU64, Ordering},
    },
    task::{Context, Poll},
    time::{Duration, Instant, SystemTime},
};

use rustorr_cache::{Cache, Pin as CachePin};
use rustorr_domain::{FileIndex, InfoHash};
use rustorr_engine::{
    AddOptions, Engine, Error as EngineError, TorrentReader, TorrentSource, TorrentStatus,
};
use rustorr_state::{CatalogEntry, Error as StateError, State};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    sync::{Mutex, oneshot},
    task::JoinHandle,
};
use tracing::{info, warn};

const PREFETCH_START_DELAY: Duration = Duration::from_millis(50);

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error("engine operation failed")]
    Engine(#[from] EngineError),
    #[error("state operation failed")]
    State(#[from] StateError),
    #[error("cache operation failed")]
    Cache(#[from] rustorr_cache::Error),
    #[error("file index {index} is unavailable for torrent {hash}")]
    UnknownFile { hash: InfoHash, index: u32 },
    #[error("R5 accepts one-based file indexes; got {0}")]
    InvalidIndex(u32),
    #[error("cannot read local torrent source")]
    LocalSource(#[source] io::Error),
}

/// Reader returned by the lifecycle module. Its pin lives as long as the HTTP
/// body does, preventing eviction under a live stream.
pub struct PlaybackReader {
    reader: TorrentReader,
    pin: Option<CachePin>,
    coordinator: Weak<TorrentCoordinator>,
}

impl PlaybackReader {
    pub fn file_length(&self) -> u64 {
        self.reader.file_length()
    }
}

impl AsyncRead for PlaybackReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.reader).poll_read(context, buffer)
    }
}

impl Drop for PlaybackReader {
    fn drop(&mut self) {
        drop(self.pin.take());
        if let Some(coordinator) = self.coordinator.upgrade()
            && let Ok(runtime) = tokio::runtime::Handle::try_current()
        {
            runtime.spawn(async move {
                if let Err(error) = coordinator.evict_to_cap().await {
                    warn!(error = %error, "cache cap enforcement after reader release failed");
                }
            });
        }
    }
}

struct PrefetchTask {
    id: u64,
    handle: JoinHandle<()>,
}

/// A deep module: callers use add/list/play/drop/evict without knowing how a
/// librqbit session, SQLite and the cache must be ordered.
pub struct TorrentCoordinator {
    engine: Arc<dyn Engine>,
    cache: Arc<Cache>,
    state: Arc<State>,
    gate: Mutex<()>,
    prefetches: Mutex<HashMap<InfoHash, PrefetchTask>>,
    peer_hints: Mutex<HashMap<InfoHash, Vec<std::net::SocketAddr>>>,
    next_prefetch_id: AtomicU64,
}

impl TorrentCoordinator {
    pub fn new(engine: Arc<dyn Engine>, cache: Arc<Cache>, state: Arc<State>) -> Self {
        Self {
            engine,
            cache,
            state,
            gate: Mutex::new(()),
            prefetches: Mutex::new(HashMap::new()),
            peer_hints: Mutex::new(HashMap::new()),
            next_prefetch_id: AtomicU64::new(1),
        }
    }

    pub fn list(&self) -> Result<Vec<CatalogEntry>, Error> {
        Ok(self.state.list_torrents()?)
    }

    pub async fn status(&self, hash: InfoHash) -> TorrentStatus {
        self.engine.torrent_status(hash).await.unwrap_or_default()
    }

    /// Adds a known torrent or resolves a magnet. Nothing reaches SQLite until
    /// librqbit has produced complete metadata and canonical torrent bytes.
    pub async fn add_link(&self, link: &str) -> Result<CatalogEntry, Error> {
        let _gate = self.gate.lock().await;
        let source = if let Some(path) = link.strip_prefix("file://") {
            TorrentSource::TorrentBytes(tokio::fs::read(path).await.map_err(Error::LocalSource)?)
        } else if link.starts_with("magnet:") {
            TorrentSource::Magnet(link.to_owned())
        } else {
            TorrentSource::Url(link.to_owned())
        };
        let source_hash = self.engine.source_hash(&source);
        if let Some(hash) = source_hash
            && let Some(entry) = self.state.torrent(hash)?
        {
            let started = Instant::now();
            self.load(hash).await?;
            info!(
                %hash,
                readd_ms = started.elapsed().as_millis(),
                "existing torrent attached"
            );
            return Ok(entry);
        }
        let initial_peers = match source_hash {
            Some(hash) => self
                .peer_hints
                .lock()
                .await
                .get(&hash)
                .cloned()
                .unwrap_or_default(),
            None => Vec::new(),
        };
        let started = Instant::now();
        let metadata = self
            .engine
            .add(
                source,
                AddOptions {
                    initial_peers: initial_peers.clone(),
                    ..AddOptions::default()
                },
            )
            .await?;
        self.peer_hints.lock().await.remove(&metadata.hash);
        if let Some(entry) = self.state.torrent(metadata.hash)? {
            info!(
                hash = %entry.hash,
                peer_hints = initial_peers.len(),
                readd_ms = started.elapsed().as_millis(),
                "existing torrent attached"
            );
            return Ok(entry);
        }
        let entry = CatalogEntry {
            hash: metadata.hash,
            title: String::new(),
            poster: String::new(),
            category: String::new(),
            data: String::new(),
            added_at: SystemTime::now(),
            size: metadata.file_lengths.into_iter().sum(),
        };
        self.state.save_torrent(&entry, &metadata.metainfo)?;
        info!(hash = %entry.hash, size = entry.size, "torrent added");
        Ok(entry)
    }

    async fn load(&self, hash: InfoHash) -> Result<(), Error> {
        if self.engine.is_loaded(hash) {
            return Ok(());
        }
        let metainfo = self
            .state
            .metainfo(hash)?
            .ok_or(EngineError::NotLoaded(hash))?;
        let initial_peers = self
            .peer_hints
            .lock()
            .await
            .get(&hash)
            .cloned()
            .unwrap_or_default();
        let started = Instant::now();
        let metadata = self
            .engine
            .add(
                TorrentSource::TorrentBytes(metainfo),
                AddOptions {
                    initial_peers: initial_peers.clone(),
                    ..AddOptions::default()
                },
            )
            .await?;
        if metadata.hash != hash {
            return Err(EngineError::NotLoaded(hash).into());
        }
        self.peer_hints.lock().await.remove(&hash);
        info!(
            %hash,
            peer_hints = initial_peers.len(),
            readd_ms = started.elapsed().as_millis(),
            "torrent lazily loaded"
        );
        Ok(())
    }

    /// Opens a one-based R1 file index at `offset` and holds a cache pin until
    /// the returned reader is dropped.
    pub async fn play(
        self: &Arc<Self>,
        hash: InfoHash,
        index: u32,
        offset: u64,
    ) -> Result<PlaybackReader, Error> {
        self.play_with_prefetch(hash, index, offset, None).await
    }

    /// Opens a playback reader and, while the lifecycle gate is still held,
    /// registers an optional look-ahead task owned by this coordinator.
    pub async fn play_with_prefetch(
        self: &Arc<Self>,
        hash: InfoHash,
        index: u32,
        offset: u64,
        prefetch_offset: Option<u64>,
    ) -> Result<PlaybackReader, Error> {
        let file = index.checked_sub(1).ok_or(Error::InvalidIndex(index))?;
        let file = FileIndex::from_zero_based(file);
        let _gate = self.gate.lock().await;
        self.load(hash).await?;
        let reader = self
            .engine
            .reader(hash, file, offset)
            .await
            .map_err(|error| match error {
                EngineError::NotLoaded(_) => Error::UnknownFile { hash, index },
                other => Error::Engine(other),
            })?;
        let pin = self.cache.pin(hash)?;
        if let Some(prefetch_offset) = prefetch_offset {
            self.start_prefetch_locked(hash, file, prefetch_offset)
                .await;
        }
        Ok(PlaybackReader {
            reader,
            pin: Some(pin),
            coordinator: Arc::downgrade(self),
        })
    }

    async fn start_prefetch_locked(self: &Arc<Self>, hash: InfoHash, file: FileIndex, offset: u64) {
        self.stop_prefetch_locked(hash).await;
        let id = self.next_prefetch_id.fetch_add(1, Ordering::Relaxed);
        let weak = Arc::downgrade(self);
        let (start, started) = oneshot::channel();
        let handle = tokio::spawn(async move {
            let _ = started.await;
            // Let the HTTP body poll its playback reader first, so librqbit
            // sees foreground demand before look-ahead demand on another
            // runtime worker.
            tokio::time::sleep(PREFETCH_START_DELAY).await;
            let Some(coordinator) = weak.upgrade() else {
                return;
            };
            let prepared = async {
                let piece_length = coordinator.engine.piece_length(hash).ok()?;
                let reader = coordinator.engine.reader(hash, file, offset).await.ok()?;
                let length = reader.file_length();
                if offset >= length {
                    return None;
                }
                let pin = coordinator.cache.pin(hash).ok()?;
                let count =
                    usize::try_from((length - offset).min(piece_length)).unwrap_or(usize::MAX);
                Some((reader, pin, vec![0; count]))
            }
            .await;
            let outcome = if let Some((mut reader, pin, mut bytes)) = prepared {
                let outcome = reader.read_exact(&mut bytes).await;
                drop(pin);
                drop(reader);
                Some(outcome)
            } else {
                None
            };
            let mut tasks = coordinator.prefetches.lock().await;
            if tasks.get(&hash).is_some_and(|task| task.id == id) {
                tasks.remove(&hash);
            }
            drop(tasks);
            if let Some(Err(error)) = outcome {
                warn!(%hash, offset, error = %error, "one-piece prefetch did not complete");
            }
            if let Err(error) = coordinator.evict_to_cap().await {
                warn!(%hash, error = %error, "cache cap enforcement after prefetch failed");
            }
        });
        self.prefetches
            .lock()
            .await
            .insert(hash, PrefetchTask { id, handle });
        let _ = start.send(());
    }

    /// Registers one-piece look-ahead under the same lifecycle gate used by
    /// deletion. The returned future only waits for task registration, not
    /// for the download itself.
    pub async fn schedule_prefetch(self: &Arc<Self>, hash: InfoHash, index: u32, offset: u64) {
        let Some(file) = index.checked_sub(1) else {
            return;
        };
        let file = FileIndex::from_zero_based(file);
        let _gate = self.gate.lock().await;
        self.start_prefetch_locked(hash, file, offset).await;
    }

    async fn stop_prefetch_locked(&self, hash: InfoHash) {
        let task = self.prefetches.lock().await.remove(&hash);
        if let Some(task) = task {
            let id = task.id;
            task.handle.abort();
            let _ = task.handle.await;
            info!(%hash, prefetch_id = id, "prefetch stopped before lifecycle change");
        }
    }

    fn ensure_unpinned(&self, hash: InfoHash) -> Result<(), Error> {
        match self.cache.ensure_unpinned(hash) {
            Ok(()) | Err(rustorr_cache::Error::UnknownTorrent(_)) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    /// Explicit product removal: release the engine, then data, then catalog.
    pub async fn drop_torrent(&self, hash: InfoHash) -> Result<bool, Error> {
        let _gate = self.gate.lock().await;
        // After a process restart the catalog and disk manifest exist before
        // the in-memory cache registry does. Attach once so wipe can remove
        // recovered bytes instead of merely forgetting the SQLite row.
        if !self.engine.is_loaded(hash) && self.state.torrent(hash)?.is_some() {
            self.load(hash).await?;
        }
        self.stop_prefetch_locked(hash).await;
        self.ensure_unpinned(hash)?;
        match self.engine.delete(hash).await {
            Ok(_) | Err(EngineError::NotLoaded(_)) => {}
            Err(error) => return Err(error.into()),
        }
        match self.cache.remove(hash) {
            Ok(_) | Err(rustorr_cache::Error::UnknownTorrent(_)) => {}
            Err(error) => return Err(error.into()),
        }
        let removed = self.state.remove_torrent(hash)?;
        let discarded_peer_hints = self
            .peer_hints
            .lock()
            .await
            .remove(&hash)
            .map_or(0, |peers| peers.len());
        let stats = self.cache.stats();
        info!(
            %hash,
            removed,
            discarded_peer_hints,
            remaining_bytes = stats.stored_bytes,
            cap_bytes = stats.cap_bytes,
            pinned_torrents = stats.pinned_torrents,
            "torrent dropped"
        );
        Ok(removed)
    }

    /// Testable, deterministic torrent-granularity eviction. The catalog is
    /// deliberately retained so future playback can lazily re-add it.
    pub async fn evict(&self, hash: InfoHash) -> Result<u64, Error> {
        let _gate = self.gate.lock().await;
        self.evict_locked(hash, "deterministic").await
    }

    async fn evict_locked(&self, hash: InfoHash, reason: &'static str) -> Result<u64, Error> {
        self.stop_prefetch_locked(hash).await;
        self.ensure_unpinned(hash)?;
        let deleted = self.engine.delete(hash).await?;
        let peer_hint_count = deleted.live_peers.len();
        if peer_hint_count > 0 {
            self.peer_hints
                .lock()
                .await
                .insert(hash, deleted.live_peers);
        }
        let freed = self.cache.remove(hash)?;
        let stats = self.cache.stats();
        info!(
            %hash,
            eviction_reason = reason,
            freed_bytes = freed,
            remaining_bytes = stats.stored_bytes,
            cap_bytes = stats.cap_bytes,
            pinned_torrents = stats.pinned_torrents,
            peer_hints = peer_hint_count,
            "torrent evicted"
        );
        Ok(freed)
    }

    /// Applies the cache's LRU policy without ever evicting a pinned reader.
    pub async fn evict_to_cap(&self) -> Result<(), Error> {
        let _gate = self.gate.lock().await;
        let candidates = self.cache.eviction_candidates();
        if !candidates.is_empty() {
            info!(candidates = ?candidates, "cache eviction candidates");
        }
        for hash in candidates {
            if let Err(error) = self.evict_locked(hash, "soft_cap").await {
                warn!(%hash, error = %error, "cache eviction failed");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashSet,
        net::{IpAddr, Ipv4Addr, SocketAddr},
        sync::{
            Arc, Mutex as StdMutex,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use rustorr_cache::{CacheConfig, MemoryStore, TorrentLayout};
    use rustorr_engine::{AddOptions, DeletedTorrent, EngineFuture, EngineStatus, TorrentMetadata};

    use super::*;

    struct FakeEngine {
        cache: Arc<Cache>,
        status: EngineStatus,
        loaded: StdMutex<HashSet<InfoHash>>,
        add_options: StdMutex<Vec<(InfoHash, AddOptions)>>,
        blocked_writers: StdMutex<Vec<tokio::io::DuplexStream>>,
        reader_offsets: StdMutex<Vec<u64>>,
        fail_delete: AtomicBool,
        block_prefetch: AtomicBool,
        deletes: AtomicUsize,
        live_peers: usize,
    }

    impl FakeEngine {
        fn new(cache: Arc<Cache>) -> Self {
            Self {
                cache,
                status: EngineStatus {
                    dht_enabled: false,
                    listen_port: None,
                },
                loaded: StdMutex::new(HashSet::new()),
                add_options: StdMutex::new(Vec::new()),
                blocked_writers: StdMutex::new(Vec::new()),
                reader_offsets: StdMutex::new(Vec::new()),
                fail_delete: AtomicBool::new(false),
                block_prefetch: AtomicBool::new(false),
                deletes: AtomicUsize::new(0),
                live_peers: 2,
            }
        }

        fn hash(byte: u8) -> InfoHash {
            InfoHash::from_bytes([byte; 20])
        }

        fn source_byte(source: &TorrentSource) -> u8 {
            match source {
                TorrentSource::TorrentBytes(bytes) => bytes[0],
                TorrentSource::Magnet(_) | TorrentSource::Url(_) => 1,
            }
        }
    }

    impl Engine for FakeEngine {
        fn status(&self) -> &EngineStatus {
            &self.status
        }

        fn is_loaded(&self, hash: InfoHash) -> bool {
            self.loaded.lock().unwrap().contains(&hash)
        }

        fn source_hash(&self, source: &TorrentSource) -> Option<InfoHash> {
            Some(Self::hash(Self::source_byte(source)))
        }

        fn add(
            &self,
            source: TorrentSource,
            options: AddOptions,
        ) -> EngineFuture<'_, TorrentMetadata> {
            Box::pin(async move {
                let byte = Self::source_byte(&source);
                let hash = Self::hash(byte);
                self.cache
                    .open(hash, TorrentLayout::new(16, vec![100]).unwrap())
                    .unwrap();
                self.loaded.lock().unwrap().insert(hash);
                self.add_options.lock().unwrap().push((hash, options));
                Ok(TorrentMetadata {
                    hash,
                    metainfo: vec![byte],
                    file_lengths: vec![100],
                })
            })
        }

        fn reader(
            &self,
            hash: InfoHash,
            _file: FileIndex,
            offset: u64,
        ) -> EngineFuture<'_, TorrentReader> {
            Box::pin(async move {
                self.reader_offsets.lock().unwrap().push(offset);
                if !self.is_loaded(hash) {
                    return Err(EngineError::NotLoaded(hash));
                }
                if offset > 0 && self.block_prefetch.load(Ordering::Relaxed) {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                    let (reader, writer) = tokio::io::duplex(16);
                    self.blocked_writers.lock().unwrap().push(writer);
                    Ok(TorrentReader::new(reader, 100))
                } else {
                    Ok(TorrentReader::new(tokio::io::empty(), 100))
                }
            })
        }

        fn piece_length(&self, hash: InfoHash) -> Result<u64, EngineError> {
            self.is_loaded(hash)
                .then_some(16)
                .ok_or(EngineError::NotLoaded(hash))
        }

        fn torrent_status(&self, hash: InfoHash) -> EngineFuture<'_, TorrentStatus> {
            Box::pin(async move {
                if !self.is_loaded(hash) {
                    return Err(EngineError::NotLoaded(hash));
                }
                Ok(TorrentStatus {
                    ready: true,
                    live_peers: self.live_peers,
                })
            })
        }

        fn delete(&self, hash: InfoHash) -> EngineFuture<'_, DeletedTorrent> {
            Box::pin(async move {
                self.deletes.fetch_add(1, Ordering::Relaxed);
                if self.fail_delete.load(Ordering::Relaxed) {
                    return Err(EngineError::Start(Box::new(io::Error::other(
                        "injected delete failure",
                    ))));
                }
                if !self.loaded.lock().unwrap().remove(&hash) {
                    return Err(EngineError::NotLoaded(hash));
                }
                Ok(DeletedTorrent {
                    live_peers: vec![SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 6881)],
                })
            })
        }
    }

    struct Fixture {
        cache: Arc<Cache>,
        state: Arc<State>,
        engine: Arc<FakeEngine>,
        coordinator: Arc<TorrentCoordinator>,
        dir: tempfile::TempDir,
    }

    impl Fixture {
        fn new(cap_bytes: u64) -> Self {
            let cache = Arc::new(Cache::new(
                Arc::new(MemoryStore::new()),
                CacheConfig { cap_bytes },
            ));
            let state = Arc::new(State::open_in_memory().unwrap());
            let engine = Arc::new(FakeEngine::new(Arc::clone(&cache)));
            let engine_port: Arc<dyn Engine> = engine.clone();
            let coordinator = Arc::new(TorrentCoordinator::new(
                engine_port,
                Arc::clone(&cache),
                Arc::clone(&state),
            ));
            Self {
                cache,
                state,
                engine,
                coordinator,
                dir: tempfile::tempdir().unwrap(),
            }
        }

        async fn add(&self, byte: u8) -> CatalogEntry {
            let path = self.dir.path().join(format!("{byte}.torrent"));
            std::fs::write(&path, [byte]).unwrap();
            self.coordinator
                .add_link(&format!("file://{}", path.display()))
                .await
                .unwrap()
        }
    }

    #[tokio::test]
    async fn active_reader_refuses_eviction_before_engine_delete() {
        let fixture = Fixture::new(1_000);
        let entry = fixture.add(1).await;
        let reader = fixture.coordinator.play(entry.hash, 1, 0).await.unwrap();

        assert!(matches!(
            fixture.coordinator.evict(entry.hash).await,
            Err(Error::Cache(rustorr_cache::Error::Pinned(_)))
        ));
        assert_eq!(fixture.engine.deletes.load(Ordering::Relaxed), 0);
        assert!(fixture.engine.is_loaded(entry.hash));
        assert!(fixture.state.torrent(entry.hash).unwrap().is_some());

        drop(reader);
        fixture.coordinator.evict(entry.hash).await.unwrap();
    }

    #[tokio::test]
    async fn prefetch_is_cancelled_and_joined_before_eviction() {
        let fixture = Fixture::new(1_000);
        fixture.engine.block_prefetch.store(true, Ordering::Relaxed);
        let entry = fixture.add(1).await;
        let reader = fixture.coordinator.play(entry.hash, 1, 0).await.unwrap();
        fixture
            .coordinator
            .schedule_prefetch(entry.hash, 1, 10)
            .await;
        drop(reader);
        tokio::task::yield_now().await;

        fixture.coordinator.evict(entry.hash).await.unwrap();

        assert_eq!(fixture.cache.stats().pinned_torrents, 0);
        assert!(!fixture.engine.is_loaded(entry.hash));
    }

    #[tokio::test]
    async fn prefetch_gives_the_playback_reader_a_head_start() {
        let fixture = Fixture::new(1_000);
        let entry = fixture.add(1).await;
        let _reader = fixture.coordinator.play(entry.hash, 1, 0).await.unwrap();

        fixture
            .coordinator
            .schedule_prefetch(entry.hash, 1, 10)
            .await;
        assert_eq!(*fixture.engine.reader_offsets.lock().unwrap(), vec![0]);

        tokio::time::sleep(PREFETCH_START_DELAY + Duration::from_millis(25)).await;
        assert_eq!(*fixture.engine.reader_offsets.lock().unwrap(), vec![0, 10]);
    }

    #[tokio::test]
    async fn delete_failure_preserves_engine_cache_and_catalog() {
        let fixture = Fixture::new(1_000);
        let entry = fixture.add(1).await;
        fixture
            .cache
            .write(entry.hash, FileIndex::from_zero_based(0), 0, &[7; 20])
            .unwrap();
        fixture.engine.fail_delete.store(true, Ordering::Relaxed);

        assert!(matches!(
            fixture.coordinator.evict(entry.hash).await,
            Err(Error::Engine(_))
        ));
        assert!(fixture.engine.is_loaded(entry.hash));
        assert_eq!(fixture.cache.stats().stored_bytes, 20);
        assert!(fixture.state.torrent(entry.hash).unwrap().is_some());
    }

    #[tokio::test]
    async fn eviction_peers_are_used_for_lazy_readd() {
        let fixture = Fixture::new(1_000);
        let entry = fixture.add(1).await;
        fixture.coordinator.evict(entry.hash).await.unwrap();

        fixture.add(1).await;

        let options = fixture.engine.add_options.lock().unwrap();
        assert_eq!(options.len(), 2);
        assert_eq!(options[1].1.initial_peers.len(), 1);
    }

    #[tokio::test]
    async fn duplicate_add_keeps_original_added_at_and_runtime_status() {
        let fixture = Fixture::new(1_000);
        let first = fixture.add(1).await;
        let stored = fixture.state.torrent(first.hash).unwrap().unwrap();

        let second = fixture.add(1).await;

        assert_eq!(second.added_at, stored.added_at);
        assert_eq!(fixture.engine.add_options.lock().unwrap().len(), 1);
        assert_eq!(
            fixture.coordinator.status(first.hash).await,
            TorrentStatus {
                ready: true,
                live_peers: 2,
            }
        );
    }

    #[tokio::test]
    async fn drop_after_restart_attaches_before_removing_recovered_cache() {
        let fixture = Fixture::new(1_000);
        let entry = fixture.add(1).await;
        fixture
            .cache
            .write(entry.hash, FileIndex::from_zero_based(0), 0, &[7; 20])
            .unwrap();
        fixture.engine.loaded.lock().unwrap().remove(&entry.hash);

        assert!(fixture.coordinator.drop_torrent(entry.hash).await.unwrap());

        assert!(fixture.state.torrent(entry.hash).unwrap().is_none());
        assert_eq!(fixture.cache.stats().torrents, 0);
        assert_eq!(fixture.engine.add_options.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn releasing_reader_enforces_soft_cap() {
        let fixture = Fixture::new(50);
        let first = fixture.add(1).await;
        fixture
            .cache
            .write(first.hash, FileIndex::from_zero_based(0), 0, &[1; 40])
            .unwrap();
        let second = fixture.add(2).await;
        fixture
            .cache
            .write(second.hash, FileIndex::from_zero_based(0), 0, &[2; 40])
            .unwrap();
        let reader = fixture.coordinator.play(second.hash, 1, 0).await.unwrap();

        drop(reader);
        tokio::time::timeout(Duration::from_secs(1), async {
            while fixture.cache.stats().stored_bytes > 50 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();

        assert!(!fixture.engine.is_loaded(first.hash));
        assert!(fixture.engine.is_loaded(second.hash));
    }
}
