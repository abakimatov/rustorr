//! The cache storage against a real librqbit session, entirely on loopback.
//!
//! A seeder session on the plain filesystem serves a small torrent; the client
//! session stores everything in a Rustorr [`Cache`]. These tests exist because
//! the storage contract (reads of missing bytes fail, storage is created again
//! on every re-add) was first derived from reading librqbit's source.

use std::{
    io::SeekFrom,
    net::{Ipv4Addr, SocketAddr},
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use librqbit::{
    AddTorrent, AddTorrentOptions, AddTorrentResponse, CreateTorrentOptions, ListenerMode,
    ListenerOptions, ManagedTorrent, Session, SessionOptions, create_torrent,
    spawn_utils::BlockingSpawner, storage::StorageFactoryExt,
};
use rustorr_cache::{Cache, CacheConfig, DiskStore, MemoryStore, TorrentLayout};
use rustorr_domain::{InfoHash, PieceIndex};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::CacheStorageFactory;

const PIECE_LENGTH: u32 = 32 * 1024;
const FILE_LENGTH: usize = 300_000;
const TIMEOUT: Duration = Duration::from_secs(60);

struct Fixture {
    _dir: tempfile::TempDir,
    content_dir: std::path::PathBuf,
    torrent: Vec<u8>,
    hash: InfoHash,
    data: Vec<u8>,
}

impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let content_dir = dir.path().join("content");
        std::fs::create_dir(&content_dir).unwrap();

        // Every piece differs, so bytes landing in the wrong place are caught.
        let mut state = 0x2545_f491_4f6c_dd1d_u64;
        let data: Vec<u8> = (0..FILE_LENGTH)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state >> 24) as u8
            })
            .collect();
        let file = content_dir.join("payload.bin");
        std::fs::write(&file, &data).unwrap();

        let created = create_torrent(
            &file,
            CreateTorrentOptions {
                name: None,
                trackers: vec![],
                piece_length: Some(PIECE_LENGTH),
            },
            &BlockingSpawner::new(2),
        )
        .await
        .unwrap();

        Self {
            hash: InfoHash::from_bytes(created.info_hash().0),
            torrent: created.as_bytes().unwrap().to_vec(),
            content_dir,
            data,
            _dir: dir,
        }
    }

    fn layout(&self) -> TorrentLayout {
        TorrentLayout::new(u64::from(PIECE_LENGTH), vec![self.data.len() as u64]).unwrap()
    }
}

fn cache() -> Arc<Cache> {
    Arc::new(Cache::new(
        Arc::new(MemoryStore::new()),
        CacheConfig::default(),
    ))
}

async fn session(scratch: &Path, listen: bool) -> Arc<Session> {
    Session::new_with_opts(
        scratch.to_path_buf(),
        SessionOptions {
            dht: None,
            disable_trackers: true,
            disable_local_service_discovery: true,
            listen: listen.then(|| ListenerOptions {
                mode: ListenerMode::TcpOnly,
                listen_addr: (Ipv4Addr::LOCALHOST, 0).into(),
                ..ListenerOptions::default()
            }),
            ..SessionOptions::default()
        },
    )
    .await
    .unwrap()
}

/// A seeder holding the complete file on the filesystem.
async fn seeder(fixture: &Fixture, scratch: &Path) -> (Arc<Session>, SocketAddr) {
    seed(fixture.torrent.clone(), &fixture.content_dir, scratch).await
}

/// A seeder of `torrent` whose files are in `folder`.
async fn seed(torrent: Vec<u8>, folder: &Path, scratch: &Path) -> (Arc<Session>, SocketAddr) {
    let session = session(scratch, true).await;
    let added = session
        .add_torrent(
            AddTorrent::from_bytes(torrent),
            Some(AddTorrentOptions {
                output_folder: Some(folder.to_string_lossy().into_owned()),
                overwrite: true,
                ..AddTorrentOptions::default()
            }),
        )
        .await
        .unwrap();
    let handle = added.into_handle().unwrap();
    tokio::time::timeout(TIMEOUT, handle.wait_until_initialized())
        .await
        .expect("seeder initialization timed out")
        .unwrap();
    let address = session.listen_addr().expect("seeder listens");
    (session, address)
}

/// Adds the torrent with its storage in `cache` and waits for initialization.
async fn add_to_cache(
    session: &Arc<Session>,
    fixture: &Fixture,
    cache: &Arc<Cache>,
    peers: Vec<SocketAddr>,
) -> (usize, Arc<ManagedTorrent>) {
    let response = session
        .add_torrent(
            AddTorrent::from_bytes(fixture.torrent.clone()),
            Some(AddTorrentOptions {
                storage_factory: Some(CacheStorageFactory::new(cache.clone()).boxed()),
                initial_peers: (!peers.is_empty()).then_some(peers),
                ..AddTorrentOptions::default()
            }),
        )
        .await
        .unwrap();
    let AddTorrentResponse::Added(id, handle) = response else {
        panic!("the torrent was expected to be added fresh");
    };
    tokio::time::timeout(TIMEOUT, handle.wait_until_initialized())
        .await
        .expect("client initialization timed out")
        .unwrap();
    (id, handle)
}

async fn read_file(handle: &Arc<ManagedTorrent>) -> Vec<u8> {
    let mut stream = handle.clone().stream(0).await.unwrap();
    let mut bytes = vec![0; FILE_LENGTH];
    tokio::time::timeout(TIMEOUT, stream.read_exact(&mut bytes))
        .await
        .expect("streaming timed out")
        .unwrap();
    bytes
}

fn all_pieces_resident(cache: &Cache, fixture: &Fixture) -> bool {
    (0..fixture.layout().piece_count()).all(|piece| {
        cache
            .is_piece_resident(fixture.hash, PieceIndex::new(piece))
            .unwrap()
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn downloads_from_a_peer_through_the_cache() {
    let fixture = Fixture::new().await;
    let scratch = tempfile::tempdir().unwrap();
    let (seeder_session, address) = seeder(&fixture, &scratch.path().join("seeder")).await;
    let cache = cache();
    let client = session(&scratch.path().join("client"), false).await;

    let (_, handle) = add_to_cache(&client, &fixture, &cache, vec![address]).await;
    let bytes = read_file(&handle).await;

    assert!(
        bytes == fixture.data,
        "streamed bytes differ from the source"
    );
    let stats = cache.stats();
    assert_eq!(stats.stored_bytes, fixture.data.len() as u64);
    assert!(
        all_pieces_resident(&cache, &fixture),
        "every piece the engine verified must be in the residency index"
    );

    client.stop().await;
    seeder_session.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn adopts_data_already_in_the_cache_without_any_peer() {
    let fixture = Fixture::new().await;
    let scratch = tempfile::tempdir().unwrap();
    let cache = cache();
    cache.open(fixture.hash, fixture.layout()).unwrap();
    cache
        .write(
            fixture.hash,
            rustorr_domain::FileIndex::from_zero_based(0),
            0,
            &fixture.data,
        )
        .unwrap();
    let client = session(scratch.path(), false).await;

    // `open` inside the factory must accept the layout derived from the real
    // torrent metadata as equal to the one opened above.
    let (_, handle) = add_to_cache(&client, &fixture, &cache, vec![]).await;
    let bytes = read_file(&handle).await;

    assert!(bytes == fixture.data);
    assert_eq!(cache.stats().stored_bytes, fixture.data.len() as u64);
    client.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_empty_cache_does_not_fail_initialization() {
    let fixture = Fixture::new().await;
    let scratch = tempfile::tempdir().unwrap();
    let cache = cache();
    let client = session(scratch.path(), false).await;

    // The initial check reads storage, every read fails, and the engine must
    // treat that as "nothing downloaded yet" rather than as an error.
    let (_, handle) = add_to_cache(&client, &fixture, &cache, vec![]).await;

    assert_eq!(cache.stats().stored_bytes, 0);
    assert!(
        !cache
            .is_piece_resident(fixture.hash, PieceIndex::new(0))
            .unwrap()
    );
    assert!(
        !handle.is_paused(),
        "the torrent must be running, not failed"
    );
    client.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn evicting_a_torrent_then_adding_it_again_fetches_it_anew() {
    let fixture = Fixture::new().await;
    let scratch = tempfile::tempdir().unwrap();
    let (seeder_session, address) = seeder(&fixture, &scratch.path().join("seeder")).await;
    let cache = cache();
    let client = session(&scratch.path().join("client"), false).await;

    let (id, handle) = add_to_cache(&client, &fixture, &cache, vec![address]).await;
    assert!(read_file(&handle).await == fixture.data);
    drop(handle);

    // The two phases of ADR 0004: the engine lets go of the torrent first,
    // and only then does the cache drop its bytes.
    client.delete(id.into(), false).await.unwrap();
    let freed = cache.remove(fixture.hash).unwrap();
    assert_eq!(freed, fixture.data.len() as u64);
    assert_eq!(cache.stats().torrents, 0);

    let started = Instant::now();
    let (_, handle) = add_to_cache(&client, &fixture, &cache, vec![address]).await;
    let bytes = read_file(&handle).await;
    eprintln!("re-fetch after eviction took {:?}", started.elapsed());

    assert!(
        bytes == fixture.data,
        "re-fetched bytes differ from the source"
    );
    assert_eq!(cache.stats().stored_bytes, fixture.data.len() as u64);
    assert!(all_pieces_resident(&cache, &fixture));

    client.stop().await;
    seeder_session.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn re_adding_without_evicting_recovers_from_the_cache_alone() {
    let fixture = Fixture::new().await;
    let scratch = tempfile::tempdir().unwrap();
    let (seeder_session, address) = seeder(&fixture, &scratch.path().join("seeder")).await;
    let cache = cache();
    let client = session(&scratch.path().join("client"), false).await;

    let (id, handle) = add_to_cache(&client, &fixture, &cache, vec![address]).await;
    assert!(read_file(&handle).await == fixture.data);
    drop(handle);
    seeder_session.stop().await;

    client.delete(id.into(), false).await.unwrap();
    let started = Instant::now();
    let (_, handle) = add_to_cache(&client, &fixture, &cache, vec![]).await;
    let bytes = read_file(&handle).await;
    eprintln!("re-add from a retained cache took {:?}", started.elapsed());

    assert!(
        bytes == fixture.data,
        "the retained cache must serve the file with no peer"
    );
    client.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_new_engine_reads_a_recovered_disk_range_without_a_peer() {
    let fixture = Fixture::new().await;
    let scratch = tempfile::tempdir().unwrap();
    let disk = tempfile::tempdir().unwrap();
    let (seeder_session, address) = seeder(&fixture, &scratch.path().join("seeder")).await;
    let first_cache = Arc::new(Cache::new(
        Arc::new(DiskStore::new(disk.path())),
        CacheConfig::default(),
    ));
    let first_client = session(&scratch.path().join("first-client"), false).await;
    let (id, handle) = add_to_cache(&first_client, &fixture, &first_cache, vec![address]).await;
    assert!(read_file(&handle).await == fixture.data);
    drop(handle);
    first_client.delete(id.into(), false).await.unwrap();
    first_client.stop().await;
    seeder_session.stop().await;
    drop(first_cache);

    let recovered_cache = Arc::new(Cache::new(
        Arc::new(DiskStore::new(disk.path())),
        CacheConfig::default(),
    ));
    let restarted_client = session(&scratch.path().join("restarted-client"), false).await;
    let (_, handle) = add_to_cache(&restarted_client, &fixture, &recovered_cache, vec![]).await;
    let offset = 70_000;
    let length = 48_000;
    let mut stream = handle.clone().stream(0).await.unwrap();
    stream.seek(SeekFrom::Start(offset as u64)).await.unwrap();
    let mut bytes = vec![0; length];
    tokio::time::timeout(TIMEOUT, stream.read_exact(&mut bytes))
        .await
        .expect("recovered range timed out")
        .unwrap();

    assert_eq!(bytes, fixture.data[offset..offset + length]);
    assert_eq!(
        recovered_cache.stats().stored_bytes,
        fixture.data.len() as u64
    );
    restarted_client.stop().await;
}

/// A magnet goes through `LibrqbitEngine`: its metadata alone first, then
/// the torrent from that metadata with the peers found (see
/// `resolve_magnet`), and the file downloads from them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_magnet_resolves_then_downloads_from_the_peer_it_found() {
    use crate::{AddOptions, Engine, EngineConfig, LibrqbitEngine, TorrentSource};
    use rustorr_domain::FileIndex;

    let fixture = Fixture::new().await;
    let scratch = tempfile::tempdir().unwrap();
    let (seeder_session, address) = seeder(&fixture, &scratch.path().join("seeder")).await;
    let engine = LibrqbitEngine::start(
        EngineConfig {
            data_dir: scratch.path().join("client"),
            listen_port: None,
            listen_ip: None,
            enable_dht: false,
            enable_trackers: false,
            proxy_url: None,
        },
        cache(),
    )
    .await
    .unwrap();

    let magnet = format!("magnet:?xt=urn:btih:{}", fixture.hash);
    let metadata = tokio::time::timeout(
        TIMEOUT,
        engine.add(
            TorrentSource::Magnet(magnet),
            AddOptions {
                initial_peers: vec![address],
                ..AddOptions::default()
            },
        ),
    )
    .await
    .expect("adding the magnet timed out")
    .unwrap();
    assert_eq!(metadata.hash, fixture.hash);
    assert_eq!(metadata.files[0].length, fixture.data.len() as u64);
    assert!(!metadata.metainfo.is_empty());

    let mut reader = engine
        .reader(fixture.hash, FileIndex::from_zero_based(0), 0)
        .await
        .unwrap();
    let mut bytes = Vec::new();
    tokio::time::timeout(TIMEOUT, reader.read_to_end(&mut bytes))
        .await
        .expect("reading timed out")
        .unwrap();
    assert!(
        bytes == fixture.data,
        "streamed bytes differ from the source"
    );

    engine.shutdown().await;
    seeder_session.stop().await;
}

/// Once a file of a torrent is read, only the files read download: one
/// episode of a season pack does not fetch the others. The download is
/// throttled so the other episode cannot complete before the read starts.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn only_the_file_being_read_is_downloaded() {
    use crate::{AddOptions, Engine, EngineConfig, LibrqbitEngine, RateLimits, TorrentSource};
    use rustorr_domain::FileIndex;

    let dir = tempfile::tempdir().unwrap();
    let season = dir.path().join("season");
    std::fs::create_dir(&season).unwrap();
    // Whole pieces per file, so no piece is shared between the episodes.
    let episode_length = 10 * PIECE_LENGTH as usize;
    let episodes: Vec<Vec<u8>> = (1..=2u8)
        .map(|n| {
            (0..episode_length)
                .map(|i| (i as u8).wrapping_mul(n).wrapping_add(n))
                .collect()
        })
        .collect();
    for (n, data) in episodes.iter().enumerate() {
        std::fs::write(season.join(format!("e{}.bin", n + 1)), data).unwrap();
    }
    let created = create_torrent(
        &season,
        CreateTorrentOptions {
            name: None,
            trackers: vec![],
            piece_length: Some(PIECE_LENGTH),
        },
        &BlockingSpawner::new(2),
    )
    .await
    .unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let (seeder_session, address) = seed(
        created.as_bytes().unwrap().to_vec(),
        &season,
        &scratch.path().join("seeder"),
    )
    .await;
    let cache = cache();
    let engine = LibrqbitEngine::start(
        EngineConfig {
            data_dir: scratch.path().join("client"),
            listen_port: None,
            listen_ip: None,
            enable_dht: false,
            enable_trackers: false,
            proxy_url: None,
        },
        Arc::clone(&cache),
    )
    .await
    .unwrap();
    engine.set_rate_limits(RateLimits {
        download: Some(128 * 1024),
        upload: None,
    });

    let metadata = engine
        .add(
            TorrentSource::TorrentBytes(created.as_bytes().unwrap().to_vec()),
            AddOptions {
                initial_peers: vec![address],
                ..AddOptions::default()
            },
        )
        .await
        .unwrap();

    let second = metadata
        .files
        .iter()
        .position(|file| file.path.ends_with("e2.bin"))
        .unwrap();
    let mut reader = engine
        .reader(metadata.hash, FileIndex::from_zero_based(second as u32), 0)
        .await
        .unwrap();
    let mut bytes = Vec::new();
    tokio::time::timeout(TIMEOUT, reader.read_to_end(&mut bytes))
        .await
        .expect("reading timed out")
        .unwrap();
    assert!(
        bytes == episodes[1],
        "streamed bytes differ from the source"
    );
    let stored = cache.stats().stored_bytes;
    assert!(
        stored < 2 * episode_length as u64,
        "the other episode was downloaded too: {stored} bytes"
    );
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(
        cache.stats().stored_bytes,
        stored,
        "the download went on after the read file was complete"
    );

    engine.shutdown().await;
    seeder_session.stop().await;
}

/// A download limit set on a running engine slows the transfer down: the
/// 300 KB file at 64 KiB/s takes seconds instead of milliseconds.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_download_limit_throttles_the_transfer() {
    use crate::{AddOptions, Engine, EngineConfig, LibrqbitEngine, RateLimits, TorrentSource};
    use rustorr_domain::FileIndex;

    let fixture = Fixture::new().await;
    let scratch = tempfile::tempdir().unwrap();
    let (seeder_session, address) = seeder(&fixture, &scratch.path().join("seeder")).await;
    let engine = LibrqbitEngine::start(
        EngineConfig {
            data_dir: scratch.path().join("client"),
            listen_port: None,
            listen_ip: None,
            enable_dht: false,
            enable_trackers: false,
            proxy_url: None,
        },
        cache(),
    )
    .await
    .unwrap();
    engine.set_rate_limits(RateLimits {
        download: Some(64 * 1024),
        upload: None,
    });

    engine
        .add(
            TorrentSource::TorrentBytes(fixture.torrent.clone()),
            AddOptions {
                initial_peers: vec![address],
                ..AddOptions::default()
            },
        )
        .await
        .unwrap();
    let started = Instant::now();
    let mut reader = engine
        .reader(fixture.hash, FileIndex::from_zero_based(0), 0)
        .await
        .unwrap();
    let mut bytes = Vec::new();
    tokio::time::timeout(TIMEOUT, reader.read_to_end(&mut bytes))
        .await
        .expect("reading timed out")
        .unwrap();

    assert!(
        bytes == fixture.data,
        "streamed bytes differ from the source"
    );
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_millis(2_500),
        "300 KB at 64 KiB/s took only {elapsed:?}"
    );

    engine.shutdown().await;
    seeder_session.stop().await;
}
