//! Torrent lifecycle policy: the only module allowed to coordinate engine
//! sessions, Rustorr cache eviction and the persistent catalog.

mod client;
mod memory;
mod model;
mod torrs_hash;

pub use client::{
    AccessPolicy, CacheCommand, CacheView, ClientCore, ClientFuture, Playback, PlaybackRequest,
    SettingsCommand, TorrentCommand, TorrentReply, ViewedCommand, ViewedFile, WafCommand, WafLists,
};
pub use memory::InMemoryClientCore;
pub use model::{Settings, TmdbConfig, TorrentFileView, TorrentView, TorznabConfig};
pub use rustorr_domain::InfoHash;

pub fn link_info_hash(link: &str) -> Option<InfoHash> {
    let link = link.replace("&amp;", "&");
    if let Ok(hash) = link.parse() {
        return Some(hash);
    }
    if let Some(decoded) = torrs_hash::decode(&link) {
        return Some(decoded.hash);
    }
    if !link.starts_with("magnet:") {
        return None;
    }
    link.split('&').find_map(|part| {
        part.strip_prefix("magnet:?xt=urn:btih:")
            .or_else(|| part.strip_prefix("xt=urn:btih:"))
            .and_then(|hash| hash.parse().ok())
    })
}

use std::{
    collections::HashMap,
    io,
    pin::Pin,
    sync::{
        Arc, RwLock, Weak,
        atomic::{AtomicU64, Ordering},
    },
    task::{Context, Poll},
    time::{Duration, Instant, SystemTime},
};

use rustorr_cache::{Cache, Pin as CachePin, ReaderRange};
use rustorr_domain::FileIndex;
use rustorr_engine::{
    AddOptions, Engine, Error as EngineError, TorrentMetadata, TorrentReader, TorrentSource,
    TorrentStatus,
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
    #[error("torrent {0} was not found")]
    NotFound(InfoHash),
    #[error("invalid settings document")]
    InvalidSettings(#[source] serde_json::Error),
}

#[derive(Debug, Clone, Default)]
pub struct AddTorrent {
    pub link: String,
    pub title: String,
    pub poster: String,
    pub category: String,
    pub data: String,
    pub save_to_db: bool,
}

#[derive(Debug, Clone)]
pub struct UpdateTorrent {
    pub hash: InfoHash,
    pub title: String,
    pub poster: String,
    pub category: String,
    pub data: String,
}

#[derive(Debug, Clone)]
struct LiveTorrent {
    metadata: TorrentMetadata,
    title: String,
    poster: String,
    category: String,
    data: String,
    added_at: SystemTime,
}

/// Reader returned by the lifecycle module. Its pin lives as long as the HTTP
/// body does, preventing eviction under a live stream.
pub struct PlaybackReader {
    reader: TorrentReader,
    pin: Option<CachePin>,
    cache: Arc<Cache>,
    hash: InfoHash,
    file: FileIndex,
    offset: u64,
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
        let before = buffer.filled().len();
        let outcome = Pin::new(&mut self.reader).poll_read(context, buffer);
        if let Poll::Ready(Ok(())) = &outcome {
            let count = u64::try_from(buffer.filled().len().saturating_sub(before))
                .expect("read length fits in u64");
            if count != 0 {
                if let Err(error) =
                    self.cache
                        .record_demand(self.hash, self.file, self.offset, count)
                {
                    return Poll::Ready(Err(io::Error::other(error)));
                }
                self.offset = self.offset.saturating_add(count);
            }
        }
        outcome
    }
}

impl Drop for PlaybackReader {
    fn drop(&mut self) {
        drop(self.pin.take());
        if let Some(coordinator) = self.coordinator.upgrade()
            && let Ok(runtime) = tokio::runtime::Handle::try_current()
        {
            let hash = self.hash;
            runtime.spawn(async move {
                if let Err(error) = coordinator.evict_to_cap().await {
                    warn!(error = %error, "cache cap enforcement after reader release failed");
                }
                coordinator.schedule_idle_detach(hash).await;
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
    idle_detaches: Mutex<HashMap<InfoHash, PrefetchTask>>,
    peer_hints: Mutex<HashMap<InfoHash, Vec<std::net::SocketAddr>>>,
    live: Mutex<HashMap<InfoHash, LiveTorrent>>,
    settings: RwLock<Settings>,
    next_prefetch_id: AtomicU64,
    next_idle_id: AtomicU64,
}

impl TorrentCoordinator {
    pub fn new(engine: Arc<dyn Engine>, cache: Arc<Cache>, state: Arc<State>) -> Self {
        let settings = state
            .settings()
            .ok()
            .flatten()
            .and_then(|document| serde_json::from_str::<Settings>(&document).ok())
            .unwrap_or_else(|| {
                let settings = Settings {
                    cache_size: i64::try_from(cache.stats().cap_bytes).unwrap_or(i64::MAX),
                    ..Settings::default()
                };
                let _ = state
                    .set_settings(&serde_json::to_string(&settings).expect("settings serialize"));
                settings
            })
            .normalized();
        cache.set_cap_bytes(settings.cache_cap());
        // An unsaved torrent does not survive a restart, so neither does its
        // cache: nothing would ever count those bytes against the cap.
        match state.list_torrents() {
            Ok(saved) => {
                let owned = saved.into_iter().map(|entry| entry.hash).collect();
                if let Err(error) = cache.discard_unowned(&owned) {
                    warn!(error = %error, "could not discard the cache of unsaved torrents");
                }
            }
            Err(error) => warn!(error = %error, "could not list saved torrents"),
        }
        Self {
            engine,
            cache,
            state,
            gate: Mutex::new(()),
            prefetches: Mutex::new(HashMap::new()),
            idle_detaches: Mutex::new(HashMap::new()),
            peer_hints: Mutex::new(HashMap::new()),
            live: Mutex::new(HashMap::new()),
            settings: RwLock::new(settings),
            next_prefetch_id: AtomicU64::new(1),
            next_idle_id: AtomicU64::new(1),
        }
    }

    pub fn settings(&self) -> Settings {
        self.settings
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub async fn set_settings(&self, settings: Settings) -> Result<(), Error> {
        let settings = settings.normalized();
        self.state
            .set_settings(&serde_json::to_string(&settings).expect("settings serialize"))?;
        self.cache.set_cap_bytes(settings.cache_cap());
        *self
            .settings
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = settings;
        self.drop_all_live().await?;
        self.evict_to_cap().await
    }

    /// MatriX.145 reconnects its torrent client on every settings change: each
    /// live torrent is dropped under the new settings, so an unsaved one
    /// disappears and a saved one returns to its catalog row. A torrent with an
    /// active reader stays loaded; the reference would cut that stream off.
    async fn drop_all_live(&self) -> Result<(), Error> {
        let mut hashes: Vec<_> = self.live.lock().await.keys().copied().collect();
        hashes.extend(
            self.state
                .list_torrents()?
                .into_iter()
                .map(|entry| entry.hash)
                .filter(|hash| self.engine.is_loaded(*hash)),
        );
        hashes.sort_by_key(ToString::to_string);
        hashes.dedup();
        for hash in hashes {
            match self.drop_live(hash).await {
                Ok(_) => {}
                Err(Error::Cache(rustorr_cache::Error::Pinned(_))) => {
                    warn!(%hash, "settings changed while playing; torrent stays loaded");
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    pub async fn reset_settings(&self) -> Result<(), Error> {
        self.set_settings(Settings::default()).await
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
        let view = self
            .add_torrent(AddTorrent {
                link: link.to_owned(),
                save_to_db: true,
                ..AddTorrent::default()
            })
            .await?;
        let hash = view
            .hash()
            .ok_or_else(|| Error::LocalSource(io::Error::other("missing hash")))?;
        self.state.torrent(hash)?.ok_or(Error::NotFound(hash))
    }

    pub async fn add_torrent(&self, request: AddTorrent) -> Result<TorrentView, Error> {
        let mut request = request;
        request.link = request.link.replace("&amp;", "&");
        let source = if let Some(path) = request.link.strip_prefix("file://") {
            TorrentSource::TorrentBytes(tokio::fs::read(path).await.map_err(Error::LocalSource)?)
        } else if request.link.starts_with("magnet:") {
            TorrentSource::Magnet(request.link.clone())
        } else if let Some(decoded) = torrs_hash::decode(&request.link) {
            if request.title.is_empty() {
                request.title = decoded.title;
            }
            if request.poster.is_empty() {
                request.poster = decoded.poster;
            }
            if request.category.is_empty() {
                request.category = decoded.category;
            }
            let trackers = decoded
                .trackers
                .iter()
                .map(|tracker| format!("&tr={}", tracker.replace(':', "%3A").replace('/', "%2F")))
                .collect::<String>();
            TorrentSource::Magnet(format!("magnet:?xt=urn:btih:{}{}", decoded.hash, trackers))
        } else if request.link.parse::<InfoHash>().is_ok() {
            TorrentSource::Magnet(format!("magnet:?xt=urn:btih:{}", request.link))
        } else {
            TorrentSource::Url(request.link.clone())
        };
        self.add_source(request, source).await
    }

    pub async fn add_metainfo(
        &self,
        bytes: Vec<u8>,
        request: AddTorrent,
    ) -> Result<TorrentView, Error> {
        self.add_source(request, TorrentSource::TorrentBytes(bytes))
            .await
    }

    async fn add_source(
        &self,
        request: AddTorrent,
        source: TorrentSource,
    ) -> Result<TorrentView, Error> {
        let _gate = self.gate.lock().await;
        let source_hash = self.engine.source_hash(&source);
        if let Some(hash) = source_hash
            && let Some(live) = self.live.lock().await.get(&hash).cloned()
        {
            if request.save_to_db && self.state.torrent(hash)?.is_none() {
                let mut saved = live.clone();
                if saved.data.is_empty() {
                    saved.data = Self::default_data(&saved.metadata);
                }
                self.persist(&saved)?;
                self.live.lock().await.insert(hash, saved);
            }
            let mut view = self.view_live(&live).await;
            view.stat = 0;
            view.stat_string = "Torrent added".into();
            return Ok(view);
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
        let existing = self.state.torrent(metadata.hash)?;
        let live = LiveTorrent {
            title: if request.title.is_empty() {
                existing
                    .as_ref()
                    .map(|entry| entry.title.clone())
                    .filter(|title| !title.is_empty())
                    .unwrap_or_else(|| metadata.name.clone())
            } else {
                request.title
            },
            poster: existing
                .as_ref()
                .map(|entry| entry.poster.clone())
                .filter(|poster| !poster.is_empty())
                .unwrap_or(request.poster),
            category: existing
                .as_ref()
                .map(|entry| entry.category.clone())
                .filter(|category| !category.is_empty())
                .unwrap_or(request.category),
            data: existing
                .as_ref()
                .map(|entry| entry.data.clone())
                .filter(|data| !data.is_empty())
                .unwrap_or(request.data),
            added_at: existing
                .as_ref()
                .map_or_else(SystemTime::now, |entry| entry.added_at),
            metadata,
        };
        let hash = live.metadata.hash;
        info!(%hash, size = live.metadata.files.iter().map(|file| file.length).sum::<u64>(), "torrent added");
        let mut view = self.view_live(&live).await;
        let mut stored = live;
        if request.save_to_db {
            if stored.data.is_empty() {
                stored.data = Self::default_data(&stored.metadata);
            }
            self.persist(&stored)?;
        }
        self.live.lock().await.insert(hash, stored);
        view.stat = 0;
        view.stat_string = "Torrent added".into();
        Ok(view)
    }

    fn persist(&self, live: &LiveTorrent) -> Result<(), Error> {
        let entry = CatalogEntry {
            hash: live.metadata.hash,
            title: live.title.clone(),
            poster: live.poster.clone(),
            category: live.category.clone(),
            data: live.data.clone(),
            added_at: live.added_at,
            size: live.metadata.files.iter().map(|file| file.length).sum(),
        };
        self.state.save_torrent(&entry, &live.metadata.metainfo)?;
        Ok(())
    }

    fn sorted_files(metadata: &TorrentMetadata) -> Vec<TorrentFileView> {
        let mut files: Vec<_> = metadata
            .files
            .iter()
            .map(|file| TorrentFileView {
                id: 0,
                path: file.path.clone(),
                length: file.length,
                engine_index: file.engine_index,
            })
            .collect();
        files.sort_by(|left, right| left.path.cmp(&right.path));
        for (index, file) in files.iter_mut().enumerate() {
            file.id = u32::try_from(index + 1).unwrap_or(u32::MAX);
        }
        files
    }

    fn default_data(metadata: &TorrentMetadata) -> String {
        #[derive(serde::Serialize)]
        struct FilesDocument {
            #[serde(rename = "TorrServer")]
            torr_server: TorrServerFiles,
        }

        #[derive(serde::Serialize)]
        struct TorrServerFiles {
            #[serde(rename = "Files")]
            files: Vec<TorrentFileView>,
        }

        serde_json::to_string(&FilesDocument {
            torr_server: TorrServerFiles {
                files: Self::sorted_files(metadata),
            },
        })
        .expect("torrent file document is serializable")
    }

    async fn view_live(&self, live: &LiveTorrent) -> TorrentView {
        let runtime = self.status(live.metadata.hash).await;
        let mut view = TorrentView {
            title: live.title.clone(),
            category: live.category.clone(),
            poster: live.poster.clone(),
            data: (!live.data.is_empty()).then(|| live.data.clone()),
            timestamp: model::unix_seconds(live.added_at),
            name: Some(live.metadata.name.clone()),
            hash: Some(live.metadata.hash.to_string()),
            torrs_hash: None,
            stat: if runtime.ready { 3 } else { 1 },
            stat_string: if runtime.ready {
                "Torrent working".into()
            } else {
                "Torrent getting info".into()
            },
            loaded_size: (runtime.progress_bytes != 0).then_some(runtime.progress_bytes),
            torrent_size: Some(live.metadata.files.iter().map(|file| file.length).sum()),
            download_speed: (runtime.download_speed != 0).then_some(runtime.download_speed),
            upload_speed: (runtime.upload_speed != 0).then_some(runtime.upload_speed),
            total_peers: (runtime.live_peers != 0).then_some(runtime.live_peers),
            active_peers: (runtime.live_peers != 0).then_some(runtime.live_peers),
            connected_seeders: (runtime.live_peers != 0).then_some(runtime.live_peers),
            bytes_written: None,
            bytes_read: None,
            file_stats: Self::sorted_files(&live.metadata),
        };
        view.torrs_hash = torrs_hash::encode(&view, &live.metadata.trackers);
        view
    }

    fn view_saved(&self, entry: &CatalogEntry) -> Result<TorrentView, Error> {
        Ok(TorrentView {
            title: entry.title.clone(),
            category: entry.category.clone(),
            poster: entry.poster.clone(),
            data: (!entry.data.is_empty()).then(|| entry.data.clone()),
            timestamp: model::unix_seconds(entry.added_at),
            name: None,
            hash: Some(entry.hash.to_string()),
            torrs_hash: None,
            stat: 5,
            stat_string: "Torrent in db".into(),
            loaded_size: None,
            torrent_size: (entry.size != 0).then_some(entry.size),
            download_speed: None,
            upload_speed: None,
            total_peers: None,
            active_peers: None,
            connected_seeders: None,
            bytes_written: None,
            bytes_read: None,
            file_stats: Vec::new(),
        })
    }

    pub async fn list_views(&self) -> Result<Vec<TorrentView>, Error> {
        let live = self.live.lock().await.clone();
        let mut views = Vec::new();
        for torrent in live.values() {
            views.push(self.view_live(torrent).await);
        }
        for entry in self.state.list_torrents()? {
            if !live.contains_key(&entry.hash) {
                views.push(self.view_saved(&entry)?);
            }
        }
        views.sort_by(|left, right| {
            right
                .timestamp
                .cmp(&left.timestamp)
                .then_with(|| right.title.cmp(&left.title))
        });
        Ok(views)
    }

    pub async fn get_view(&self, hash: InfoHash) -> Result<Option<TorrentView>, Error> {
        if let Some(live) = self.live.lock().await.get(&hash).cloned() {
            return Ok(Some(self.view_live(&live).await));
        }
        self.state
            .torrent(hash)?
            .as_ref()
            .map(|entry| self.view_saved(entry))
            .transpose()
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
        if !self.live.lock().await.contains_key(&hash) {
            let entry = self.state.torrent(hash)?.ok_or(Error::NotFound(hash))?;
            self.live.lock().await.insert(
                hash,
                LiveTorrent {
                    metadata,
                    title: entry.title,
                    poster: entry.poster,
                    category: entry.category,
                    data: entry.data,
                    added_at: entry.added_at,
                },
            );
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
        self.play_with_prefetch(hash, index, offset, None, None)
            .await
    }

    /// Opens a playback reader and, while the lifecycle gate is still held,
    /// registers an optional look-ahead task owned by this coordinator.
    pub async fn play_with_prefetch(
        self: &Arc<Self>,
        hash: InfoHash,
        index: u32,
        offset: u64,
        end: Option<u64>,
        prefetch_offset: Option<u64>,
    ) -> Result<PlaybackReader, Error> {
        let _gate = self.gate.lock().await;
        self.stop_idle_detach_locked(hash).await;
        self.load(hash).await?;
        let file = self.engine_file(hash, index).await?;
        let reader = self
            .engine
            .reader(hash, file, offset)
            .await
            .map_err(|error| match error {
                EngineError::NotLoaded(_) => Error::UnknownFile { hash, index },
                other => Error::Engine(other),
            })?;
        let range_end = end
            .unwrap_or_else(|| reader.file_length().saturating_sub(1))
            .min(reader.file_length().saturating_sub(1));
        let pin = self.cache.pin_range(
            hash,
            ReaderRange {
                file,
                start: offset,
                end: range_end,
            },
        )?;
        if let Some(prefetch_offset) = prefetch_offset {
            self.start_prefetch_locked(hash, file, prefetch_offset)
                .await;
        }
        Ok(PlaybackReader {
            reader,
            pin: Some(pin),
            cache: Arc::clone(&self.cache),
            hash,
            file,
            offset,
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
                let preload = coordinator.settings().preload_cache;
                let budget = coordinator
                    .settings()
                    .cache_cap()
                    .saturating_mul(u64::try_from(preload).ok()?)
                    / 100;
                if budget == 0 {
                    return None;
                }
                let reader = coordinator.engine.reader(hash, file, offset).await.ok()?;
                let length = reader.file_length();
                if offset >= length {
                    return None;
                }
                let pin = coordinator.cache.pin(hash).ok()?;
                let count = usize::try_from((length - offset).min(budget)).unwrap_or(usize::MAX);
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
        let _gate = self.gate.lock().await;
        let Ok(file) = self.engine_file(hash, index).await else {
            return;
        };
        self.start_prefetch_locked(hash, file, offset).await;
    }

    async fn engine_file(&self, hash: InfoHash, index: u32) -> Result<FileIndex, Error> {
        let live = self
            .live
            .lock()
            .await
            .get(&hash)
            .cloned()
            .ok_or(Error::NotFound(hash))?;
        let file = Self::sorted_files(&live.metadata)
            .into_iter()
            .find(|file| file.id == index)
            .ok_or(Error::UnknownFile { hash, index })?;
        Ok(FileIndex::from_zero_based(file.engine_index))
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

    async fn stop_idle_detach_locked(&self, hash: InfoHash) {
        if let Some(task) = self.idle_detaches.lock().await.remove(&hash) {
            task.handle.abort();
            let _ = task.handle.await;
        }
    }

    async fn schedule_idle_detach(self: &Arc<Self>, hash: InfoHash) {
        if self.ensure_unpinned(hash).is_err() || !self.engine.is_loaded(hash) {
            return;
        }
        self.stop_idle_detach_locked(hash).await;
        let timeout = u64::try_from(self.settings().torrent_disconnect_timeout).unwrap_or_default();
        let id = self.next_idle_id.fetch_add(1, Ordering::Relaxed);
        let weak = Arc::downgrade(self);
        let handle = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(timeout)).await;
            let Some(coordinator) = weak.upgrade() else {
                return;
            };
            let mut tasks = coordinator.idle_detaches.lock().await;
            if tasks.get(&hash).is_none_or(|task| task.id != id) {
                return;
            }
            tasks.remove(&hash);
            drop(tasks);
            if let Err(error) = coordinator.drop_live(hash).await {
                warn!(%hash, error = %error, "idle torrent detach failed");
            }
        });
        self.idle_detaches
            .lock()
            .await
            .insert(hash, PrefetchTask { id, handle });
    }

    fn ensure_unpinned(&self, hash: InfoHash) -> Result<(), Error> {
        match self.cache.ensure_unpinned(hash) {
            Ok(()) | Err(rustorr_cache::Error::UnknownTorrent(_)) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    /// Explicit product removal: release the engine, then data, then catalog.
    pub async fn drop_torrent(&self, hash: InfoHash) -> Result<bool, Error> {
        self.remove_torrent(hash).await
    }

    /// TorrServer `drop`: detach the live session, retain a saved catalog row,
    /// and remove bytes only when RemoveCacheOnDrop is enabled.
    pub async fn drop_live(&self, hash: InfoHash) -> Result<bool, Error> {
        let _gate = self.gate.lock().await;
        self.stop_idle_detach_locked(hash).await;
        let saved = self.state.torrent(hash)?.is_some();
        self.stop_prefetch_locked(hash).await;
        self.ensure_unpinned(hash)?;
        if self.engine.is_loaded(hash) {
            let deleted = self.engine.delete(hash).await?;
            if !deleted.live_peers.is_empty() {
                self.peer_hints
                    .lock()
                    .await
                    .insert(hash, deleted.live_peers);
            }
        }
        let remove_cache = self.settings().remove_cache_on_drop || !saved;
        if remove_cache {
            match self.cache.remove(hash) {
                Ok(_) | Err(rustorr_cache::Error::UnknownTorrent(_)) => {}
                Err(error) => return Err(error.into()),
            }
        }
        self.live.lock().await.remove(&hash);
        Ok(saved)
    }

    /// TorrServer `rem`: delete the session, cache and catalog while leaving
    /// viewed history untouched.
    pub async fn remove_torrent(&self, hash: InfoHash) -> Result<bool, Error> {
        let _gate = self.gate.lock().await;
        self.stop_idle_detach_locked(hash).await;
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
        self.live.lock().await.remove(&hash);
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

    pub async fn wipe(&self) -> Result<(), Error> {
        let mut hashes: Vec<_> = self
            .live
            .lock()
            .await
            .keys()
            .copied()
            .chain(
                self.state
                    .list_torrents()?
                    .into_iter()
                    .map(|entry| entry.hash),
            )
            .collect();
        hashes.sort_by_key(ToString::to_string);
        hashes.dedup();
        for hash in hashes {
            self.remove_torrent(hash).await?;
        }
        Ok(())
    }

    pub async fn update_torrent(&self, update: UpdateTorrent) -> Result<(), Error> {
        let _gate = self.gate.lock().await;
        let mut live = self.live.lock().await;
        if let Some(entry) = live.get_mut(&update.hash) {
            entry.title = if update.title.is_empty() {
                entry.metadata.name.clone()
            } else {
                update.title.clone()
            };
            entry.poster = update.poster.clone();
            entry.category = update.category.clone();
            if !update.data.is_empty() {
                entry.data = update.data.clone();
            }
            if self.state.torrent(update.hash)?.is_some() {
                self.persist(entry)?;
            }
            return Ok(());
        }
        let Some(mut entry) = self.state.torrent(update.hash)? else {
            return Ok(());
        };
        let metainfo = self
            .state
            .metainfo(update.hash)?
            .ok_or(Error::NotFound(update.hash))?;
        entry.title = if update.title.is_empty() {
            self.engine.inspect_metainfo(&metainfo)?.name
        } else {
            update.title
        };
        entry.poster = update.poster;
        entry.category = update.category;
        if !update.data.is_empty() {
            entry.data = update.data;
        }
        self.state.save_torrent(&entry, &metainfo)?;
        Ok(())
    }

    /// Testable, deterministic torrent-granularity eviction. The catalog is
    /// deliberately retained so future playback can lazily re-add it.
    pub async fn evict(&self, hash: InfoHash) -> Result<u64, Error> {
        let _gate = self.gate.lock().await;
        self.evict_locked(hash, "deterministic").await
    }

    async fn evict_locked(&self, hash: InfoHash, reason: &'static str) -> Result<u64, Error> {
        self.stop_idle_detach_locked(hash).await;
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
        self.live.lock().await.remove(&hash);
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
    use rustorr_engine::{
        AddOptions, DeletedTorrent, EngineFuture, EngineStatus, TorrentFile, TorrentMetadata,
    };

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

        fn inspect_metainfo(&self, bytes: &[u8]) -> Result<TorrentMetadata, EngineError> {
            let byte = bytes[0];
            Ok(TorrentMetadata {
                hash: Self::hash(byte),
                metainfo: vec![byte],
                name: format!("{byte}.bin"),
                files: vec![TorrentFile {
                    engine_index: 0,
                    path: format!("{byte}.bin"),
                    length: 100,
                }],
                trackers: Vec::new(),
            })
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
                    name: format!("{byte}.bin"),
                    files: vec![TorrentFile {
                        engine_index: 0,
                        path: format!("{byte}.bin"),
                        length: 100,
                    }],
                    trackers: Vec::new(),
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
                    ..TorrentStatus::default()
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
    async fn idle_timeout_detaches_the_engine_but_keeps_a_saved_catalog_entry() {
        let fixture = Fixture::new(1_000);
        fixture
            .coordinator
            .set_settings(Settings {
                torrent_disconnect_timeout: 1,
                ..fixture.coordinator.settings()
            })
            .await
            .unwrap();
        let entry = fixture.add(1).await;
        let reader = fixture.coordinator.play(entry.hash, 1, 0).await.unwrap();
        drop(reader);

        tokio::time::sleep(Duration::from_millis(1_100)).await;

        assert!(!fixture.engine.is_loaded(entry.hash));
        assert!(fixture.state.torrent(entry.hash).unwrap().is_some());
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
                ..TorrentStatus::default()
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

    #[tokio::test]
    async fn a_restart_discards_the_cache_of_torrents_that_were_not_saved() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("cache");
        let state = Arc::new(State::open(dir.path().join("rustorr.db")).unwrap());
        let process = || {
            let cache = Arc::new(Cache::new(
                Arc::new(rustorr_cache::DiskStore::new(&root)),
                CacheConfig { cap_bytes: 1_000 },
            ));
            let engine: Arc<dyn Engine> = Arc::new(FakeEngine::new(Arc::clone(&cache)));
            TorrentCoordinator::new(engine, cache, Arc::clone(&state))
        };
        let first = process();
        let mut hashes = Vec::new();
        for (byte, save_to_db) in [(1, true), (2, false)] {
            let path = dir.path().join(format!("{byte}.torrent"));
            std::fs::write(&path, [byte]).unwrap();
            let view = first
                .add_torrent(AddTorrent {
                    link: format!("file://{}", path.display()),
                    save_to_db,
                    ..AddTorrent::default()
                })
                .await
                .unwrap();
            let hash = view.hash().unwrap();
            first
                .cache
                .write(hash, FileIndex::from_zero_based(0), 0, &[byte; 10])
                .unwrap();
            hashes.push(hash);
        }
        drop(first);

        let _second = process();

        assert!(root.join(hashes[0].to_string()).exists());
        assert!(!root.join(hashes[1].to_string()).exists());
    }

    #[tokio::test]
    async fn changing_settings_unloads_live_torrents_except_active_playback() {
        let fixture = Fixture::new(1_000);
        let listed = |views: Vec<TorrentView>| -> Vec<String> {
            views.into_iter().filter_map(|view| view.hash).collect()
        };
        for change in ["set", "def"] {
            let saved = fixture.add(1).await;
            let path = fixture.dir.path().join("2.torrent");
            std::fs::write(&path, [2]).unwrap();
            let unsaved = fixture
                .coordinator
                .add_torrent(AddTorrent {
                    link: format!("file://{}", path.display()),
                    ..AddTorrent::default()
                })
                .await
                .unwrap();
            let unsaved = unsaved.hash().unwrap();
            let playing = fixture.add(3).await;
            let reader = fixture.coordinator.play(playing.hash, 1, 0).await.unwrap();

            if change == "set" {
                let settings = fixture.coordinator.settings();
                fixture.coordinator.set_settings(settings).await.unwrap();
            } else {
                fixture.coordinator.reset_settings().await.unwrap();
            }

            let views = listed(fixture.coordinator.list_views().await.unwrap());
            assert!(!views.contains(&unsaved.to_string()), "{change}");
            assert!(!fixture.engine.is_loaded(unsaved), "{change}");
            assert!(views.contains(&saved.hash.to_string()), "{change}");
            assert!(!fixture.engine.is_loaded(saved.hash), "{change}");
            assert!(fixture.state.torrent(saved.hash).unwrap().is_some());
            assert!(fixture.engine.is_loaded(playing.hash), "{change}");
            drop(reader);
            fixture.coordinator.wipe().await.unwrap();
        }
    }
}
