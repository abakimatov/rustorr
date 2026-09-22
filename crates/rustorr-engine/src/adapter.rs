use std::{
    collections::HashMap,
    io::SeekFrom,
    net::Ipv6Addr,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::Duration,
};

use librqbit::{
    AddTorrent, AddTorrentOptions as LibrqbitAddOptions, AddTorrentResponse, DhtSessionConfig,
    ListenerMode, ListenerOptions, ManagedTorrent, Session, SessionOptions,
    dht::DhtPersistenceConfig, storage::StorageFactoryExt,
};
use librqbit_core::{magnet::Magnet, torrent_metainfo::torrent_from_bytes};
use rustorr_cache::Cache;
use rustorr_domain::{FileIndex, InfoHash};
use tokio::io::AsyncSeekExt;

use crate::{
    AddOptions, CacheStorageFactory, DeletedTorrent, Engine, EngineConfig, EngineFuture,
    EngineStatus, Error, TorrentMetadata, TorrentReader, TorrentSource, TorrentStatus,
};

const SCRATCH_DIR: &str = "scratch";
const DHT_CACHE_FILE: &str = "dht.json";

/// [`Engine`] backed by a `librqbit` session. The only place in Rustorr that
/// names librqbit types.
pub struct LibrqbitEngine {
    session: Arc<Session>,
    status: EngineStatus,
    loaded: Mutex<HashMap<InfoHash, usize>>,
}

impl LibrqbitEngine {
    /// Starts a session and checks that the engine applied what was asked.
    ///
    /// Run this on a multi-thread tokio runtime. librqbit moves blocking
    /// storage work off the async threads with `block_in_place`; on a
    /// current-thread runtime it instead runs that work inline and stalls the
    /// whole runtime.
    pub async fn start(config: EngineConfig, cache: Arc<Cache>) -> Result<Self, Error> {
        tokio::fs::create_dir_all(&config.data_dir)
            .await
            .map_err(|source| Error::DataDir {
                path: config.data_dir.clone(),
                source,
            })?;

        let session = Session::new_with_opts(
            config.data_dir.join(SCRATCH_DIR),
            session_options(&config, cache),
        )
        .await
        .map_err(|error| Error::Start(error.into()))?;

        let status = EngineStatus {
            dht_enabled: session.get_dht().is_some(),
            listen_port: session.listen_addr().map(|addr| addr.port()),
        };
        if let Err(error) = verify(&config, &status) {
            session.stop().await;
            return Err(error);
        }
        Ok(Self {
            session,
            status,
            loaded: Mutex::default(),
        })
    }

    /// Pauses torrents and stops the session's tasks.
    pub async fn shutdown(&self) {
        self.session.stop().await;
    }

    fn loaded(&self) -> MutexGuard<'_, HashMap<InfoHash, usize>> {
        self.loaded.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn is_loaded_inner(&self, hash: InfoHash) -> bool {
        self.loaded().contains_key(&hash)
    }

    async fn add_inner(
        &self,
        source: TorrentSource,
        options: AddOptions,
    ) -> Result<TorrentMetadata, Error> {
        let source = match source {
            TorrentSource::TorrentBytes(bytes) => AddTorrent::from_bytes(bytes),
            TorrentSource::Magnet(value) | TorrentSource::Url(value) => AddTorrent::from_url(value),
        };
        let response = self
            .session
            .add_torrent(
                source,
                Some(LibrqbitAddOptions {
                    only_files: options.only_files,
                    initial_peers: (!options.initial_peers.is_empty())
                        .then_some(options.initial_peers),
                    ..LibrqbitAddOptions::default()
                }),
            )
            .await
            .map_err(Error::Torrent)?;
        let (id, handle) = match response {
            AddTorrentResponse::Added(id, handle)
            | AddTorrentResponse::AlreadyManaged(id, handle) => (id, handle),
            AddTorrentResponse::ListOnly(_) => unreachable!("list_only was not requested"),
        };
        handle
            .wait_until_initialized()
            .await
            .map_err(Error::Torrent)?;
        let metadata = handle
            .with_metadata(|metadata| TorrentMetadata {
                hash: InfoHash::from_bytes(handle.info_hash().0),
                metainfo: metadata.torrent_bytes.to_vec(),
                file_lengths: metadata.file_infos.iter().map(|file| file.len).collect(),
            })
            .map_err(Error::Torrent)?;
        self.loaded().insert(metadata.hash, id);
        Ok(metadata)
    }

    async fn reader_inner(
        &self,
        hash: InfoHash,
        file: FileIndex,
        offset: u64,
    ) -> Result<TorrentReader, Error> {
        let id = *self.loaded().get(&hash).ok_or(Error::NotLoaded(hash))?;
        let handle = self.handle(id).ok_or(Error::NotLoaded(hash))?;
        let file_id = file.zero_based() as usize;
        let file_length = handle
            .with_metadata(|metadata| metadata.file_infos.get(file_id).map(|file| file.len))
            .map_err(Error::Torrent)?
            .ok_or(Error::NotLoaded(hash))?;
        let mut reader = handle.stream(file_id).await.map_err(Error::Torrent)?;
        reader
            .seek(SeekFrom::Start(offset))
            .await
            .map_err(|error| Error::Torrent(error.into()))?;
        Ok(TorrentReader::new(reader, file_length))
    }

    fn handle(&self, id: usize) -> Option<Arc<ManagedTorrent>> {
        self.session.with_torrents(|torrents| {
            for (candidate, handle) in torrents {
                if candidate == id {
                    return Some(Arc::clone(handle));
                }
            }
            None
        })
    }

    fn piece_length_inner(&self, hash: InfoHash) -> Result<u64, Error> {
        let id = *self.loaded().get(&hash).ok_or(Error::NotLoaded(hash))?;
        let handle = self.handle(id).ok_or(Error::NotLoaded(hash))?;
        handle
            .with_metadata(|metadata| u64::from(metadata.info.lengths().default_piece_length()))
            .map_err(Error::Torrent)
    }

    fn torrent_status_inner(&self, hash: InfoHash) -> Result<TorrentStatus, Error> {
        let id = *self.loaded().get(&hash).ok_or(Error::NotLoaded(hash))?;
        let handle = self.handle(id).ok_or(Error::NotLoaded(hash))?;
        let Some(live) = handle.live() else {
            return Ok(TorrentStatus::default());
        };
        Ok(TorrentStatus {
            ready: true,
            live_peers: live.per_peer_stats_snapshot(Default::default()).peers.len(),
        })
    }

    async fn delete_inner(&self, hash: InfoHash) -> Result<DeletedTorrent, Error> {
        let id = *self.loaded().get(&hash).ok_or(Error::NotLoaded(hash))?;
        let handle = self.handle(id).ok_or(Error::NotLoaded(hash))?;
        let live_peers = handle
            .live()
            .map(|live| {
                live.per_peer_stats_snapshot(Default::default())
                    .peers
                    .into_keys()
                    .filter_map(|address| address.parse().ok())
                    .collect()
            })
            .unwrap_or_default();
        self.session
            .delete(id.into(), false)
            .await
            .map_err(Error::Torrent)?;
        // librqbit's delete cancels peer tasks but is not a join barrier: an
        // already-received chunk can still reach storage immediately after
        // it returns. Give those cancelled tasks one scheduler grace window
        // before the coordinator is allowed to remove the cache entry.
        tokio::time::sleep(Duration::from_secs(1)).await;
        let mut loaded = self.loaded();
        if loaded.get(&hash) == Some(&id) {
            loaded.remove(&hash);
        }
        Ok(DeletedTorrent { live_peers })
    }
}

impl Engine for LibrqbitEngine {
    fn status(&self) -> &EngineStatus {
        &self.status
    }

    fn is_loaded(&self, hash: InfoHash) -> bool {
        self.is_loaded_inner(hash)
    }

    fn source_hash(&self, source: &TorrentSource) -> Option<InfoHash> {
        let id = match source {
            TorrentSource::TorrentBytes(bytes) => torrent_from_bytes(bytes).ok()?.info_hash,
            TorrentSource::Magnet(value) => Magnet::parse(value).ok()?.as_id20()?,
            TorrentSource::Url(_) => return None,
        };
        Some(InfoHash::from_bytes(id.0))
    }

    fn add(&self, source: TorrentSource, options: AddOptions) -> EngineFuture<'_, TorrentMetadata> {
        Box::pin(self.add_inner(source, options))
    }

    fn reader(
        &self,
        hash: InfoHash,
        file: FileIndex,
        offset: u64,
    ) -> EngineFuture<'_, TorrentReader> {
        Box::pin(self.reader_inner(hash, file, offset))
    }

    fn piece_length(&self, hash: InfoHash) -> Result<u64, Error> {
        self.piece_length_inner(hash)
    }

    fn torrent_status(&self, hash: InfoHash) -> EngineFuture<'_, TorrentStatus> {
        Box::pin(async move { self.torrent_status_inner(hash) })
    }

    fn delete(&self, hash: InfoHash) -> EngineFuture<'_, DeletedTorrent> {
        Box::pin(self.delete_inner(hash))
    }
}

/// The single place session options are built.
///
/// Persistence and fast-resume stay off: fast-resume was only shown to work
/// for an unchanged output directory (R3), and torrent state belongs to
/// `rustorr-state`. The DHT cache is different: it holds routing-table nodes
/// that are safe to lose, so it is kept, but at an explicit path inside
/// `data_dir` because the library default is the OS cache directory.
///
/// Trackers, persistence and fast-resume cannot be read back from a running
/// session, so they are covered by unit tests on this function instead of by
/// [`verify`].
fn session_options(config: &EngineConfig, cache: Arc<Cache>) -> SessionOptions {
    SessionOptions {
        dht: config.enable_dht.then(|| DhtSessionConfig {
            bootstrap_addrs: None,
            port: None,
            persistence: Some(DhtPersistenceConfig {
                dump_interval: None,
                config_filename: Some(config.data_dir.join(DHT_CACHE_FILE)),
            }),
        }),
        disable_trackers: !config.enable_trackers,
        fastresume: false,
        persistence: None,
        default_storage_factory: Some(CacheStorageFactory::new(cache).boxed()),
        listen: config.listen_port.map(|port| ListenerOptions {
            mode: ListenerMode::TcpAndUtp,
            listen_addr: (Ipv6Addr::UNSPECIFIED, port).into(),
            ..ListenerOptions::default()
        }),
        ..SessionOptions::default()
    }
}

fn verify(config: &EngineConfig, status: &EngineStatus) -> Result<(), Error> {
    if status.dht_enabled != config.enable_dht {
        return Err(Error::SettingNotApplied {
            setting: "dht",
            expected: config.enable_dht.to_string(),
            actual: status.dht_enabled.to_string(),
        });
    }

    let listener_applied = match (config.listen_port, status.listen_port) {
        (None, None) => true,
        (Some(0), Some(bound)) => bound != 0,
        (Some(wanted), Some(bound)) => wanted == bound,
        _ => false,
    };
    if !listener_applied {
        return Err(Error::SettingNotApplied {
            setting: "listen port",
            expected: format!("{:?}", config.listen_port),
            actual: format!("{:?}", status.listen_port),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use rustorr_cache::{CacheConfig, MemoryStore};

    fn config(data_dir: impl Into<PathBuf>) -> EngineConfig {
        EngineConfig {
            data_dir: data_dir.into(),
            listen_port: None,
            enable_dht: false,
            enable_trackers: false,
        }
    }

    fn cache() -> Arc<Cache> {
        Arc::new(Cache::new(
            Arc::new(MemoryStore::new()),
            CacheConfig::default(),
        ))
    }

    #[test]
    fn engine_persistence_and_fastresume_are_always_off() {
        for enable in [false, true] {
            let options = session_options(
                &EngineConfig {
                    enable_dht: enable,
                    enable_trackers: enable,
                    listen_port: enable.then_some(0),
                    ..config("/data")
                },
                cache(),
            );
            assert!(options.persistence.is_none());
            assert!(!options.fastresume);
        }
    }

    #[test]
    fn trackers_flag_is_applied_at_session_level() {
        let off = session_options(&config("/data"), cache());
        let on = session_options(
            &EngineConfig {
                enable_trackers: true,
                ..config("/data")
            },
            cache(),
        );
        assert!(off.disable_trackers);
        assert!(!on.disable_trackers);
    }

    #[test]
    fn dht_is_absent_when_disabled() {
        assert!(session_options(&config("/data"), cache()).dht.is_none());
    }

    #[test]
    fn dht_cache_is_pinned_inside_the_data_dir() {
        let options = session_options(
            &EngineConfig {
                enable_dht: true,
                ..config("/data")
            },
            cache(),
        );
        let persistence = options.dht.unwrap().persistence.unwrap();
        assert_eq!(
            persistence.config_filename,
            Some(PathBuf::from("/data/dht.json"))
        );
    }

    #[test]
    fn listener_serves_tcp_and_utp_on_the_requested_port() {
        assert!(session_options(&config("/data"), cache()).listen.is_none());

        let listen = session_options(
            &EngineConfig {
                listen_port: Some(51413),
                ..config("/data")
            },
            cache(),
        )
        .listen
        .unwrap();
        assert!(matches!(listen.mode, ListenerMode::TcpAndUtp));
        assert_eq!(listen.listen_addr.port(), 51413);
    }

    #[test]
    fn verify_accepts_matching_status() {
        let status = EngineStatus {
            dht_enabled: true,
            listen_port: Some(51413),
        };
        let wanted = EngineConfig {
            enable_dht: true,
            listen_port: Some(51413),
            ..config("/data")
        };
        assert!(verify(&wanted, &status).is_ok());
        // An ephemeral request is satisfied by any real port.
        let ephemeral = EngineConfig {
            listen_port: Some(0),
            ..wanted
        };
        assert!(verify(&ephemeral, &status).is_ok());
    }

    #[test]
    fn verify_reports_a_dht_that_did_not_start() {
        let status = EngineStatus {
            dht_enabled: false,
            listen_port: None,
        };
        let wanted = EngineConfig {
            enable_dht: true,
            ..config("/data")
        };
        assert!(matches!(
            verify(&wanted, &status),
            Err(Error::SettingNotApplied { setting: "dht", .. })
        ));
    }

    #[test]
    fn verify_reports_listener_mismatches() {
        let bound = |port| EngineStatus {
            dht_enabled: false,
            listen_port: port,
        };
        let listening_on = |port| EngineConfig {
            listen_port: port,
            ..config("/data")
        };
        let cases = [
            (listening_on(Some(51413)), bound(None)),
            (listening_on(None), bound(Some(51413))),
            (listening_on(Some(51413)), bound(Some(51414))),
            (listening_on(Some(0)), bound(Some(0))),
        ];
        for (wanted, status) in cases {
            assert!(
                matches!(
                    verify(&wanted, &status),
                    Err(Error::SettingNotApplied {
                        setting: "listen port",
                        ..
                    })
                ),
                "{wanted:?} vs {status:?}"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn starts_without_network_and_reports_what_it_applied() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().join("engine");

        let engine = LibrqbitEngine::start(config(&data_dir), cache())
            .await
            .unwrap();

        assert_eq!(
            engine.status(),
            &EngineStatus {
                dht_enabled: false,
                listen_port: None
            }
        );
        assert!(data_dir.is_dir());
        engine.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn binds_a_listener_when_asked() {
        let dir = tempfile::tempdir().unwrap();

        let engine = LibrqbitEngine::start(
            EngineConfig {
                listen_port: Some(0),
                ..config(dir.path())
            },
            cache(),
        )
        .await
        .unwrap();

        assert!(engine.status().listen_port.is_some_and(|port| port != 0));
        engine.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn starts_the_dht_when_asked() {
        let dir = tempfile::tempdir().unwrap();

        let engine = LibrqbitEngine::start(
            EngineConfig {
                enable_dht: true,
                ..config(dir.path())
            },
            cache(),
        )
        .await
        .unwrap();

        assert!(engine.status().dht_enabled);
        engine.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn reports_an_unusable_data_dir() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("occupied");
        std::fs::write(&file, b"").unwrap();

        let error = LibrqbitEngine::start(config(file.join("engine")), cache())
            .await
            .err()
            .unwrap();

        assert!(matches!(error, Error::DataDir { .. }), "{error}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn failed_delete_keeps_the_registry_entry() {
        let dir = tempfile::tempdir().unwrap();
        let engine = LibrqbitEngine::start(config(dir.path()), cache())
            .await
            .unwrap();
        let hash = InfoHash::from_bytes([9; 20]);
        engine.loaded().insert(hash, usize::MAX);

        assert!(Engine::delete(&engine, hash).await.is_err());
        assert!(engine.is_loaded(hash));

        engine.shutdown().await;
    }
}
