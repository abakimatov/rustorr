//! The composition root: the one place that builds the concrete engine, cache,
//! state and HTTP server, starts them in order and stops them in reverse.

use std::{future::Future, io, pin::Pin, sync::Arc, time::Duration};

use anyhow::{Context, bail};
use rustorr_cache::{Cache, CacheConfig, DiskStore, MemoryStore, PieceStore};
use rustorr_engine::{Engine, EngineConfig, LibrqbitEngine};
use rustorr_http::{Credentials, HttpConfig, Integrations, Msx, ServerInfo};
use rustorr_lifecycle::{ClientCore, TorrentCoordinator};
use rustorr_search::{RUTOR_URL, RutorDatabase, Search, SearchService};
use rustorr_state::State;
use tokio::{
    net::TcpListener,
    signal::unix::{Signal, SignalKind, signal},
    sync::{Notify, oneshot},
};
use tracing::{info, warn};

use crate::{
    config::{CacheMode, Config},
    discovery::DiscoveryService,
};

/// The version clients see in `/echo`, Bonjour TXT records and MSX.
const VERSION: &str = "MatriX.145";

pub async fn run(config: Config) -> anyhow::Result<()> {
    let started = std::time::SystemTime::now();
    // Installed first so a signal that arrives during startup is not lost.
    let mut signals = Signals::install().context("cannot install signal handlers")?;

    let http = HttpConfig {
        credentials: config
            .http_auth
            .then(|| Credentials::read(&config.data_dir.join("accs.db")))
            .transpose()
            .map_err(anyhow::Error::msg)?,
        trusted_proxies: config.trusted_proxies.clone(),
        shutdown: Some(Arc::new(Notify::new())),
        read_only: config.read_only,
        max_stream_size: config.max_stream_size,
        search_without_auth: config.search_without_auth,
        webdav: config.webdav,
    };

    let listener = TcpListener::bind(config.listen)
        .await
        .with_context(|| format!("cannot listen on {}", config.listen))?;
    let address = listener
        .local_addr()
        .context("cannot read the bound address")?;

    tokio::fs::create_dir_all(&config.data_dir)
        .await
        .with_context(|| {
            format!(
                "cannot create the data directory {}",
                config.data_dir.display()
            )
        })?;
    let state = Arc::new(open_state(&config).await?);
    let schema_version = state
        .schema_version()
        .context("cannot read the schema version")?;
    let cache = Arc::new(Cache::new(
        cache_store(&config),
        CacheConfig {
            cap_bytes: config.cache_size,
        },
    ));
    let engine = Arc::new(
        LibrqbitEngine::start(
            EngineConfig {
                data_dir: config.data_dir.join("engine"),
                listen_port: Some(config.peer_port),
                enable_dht: !config.disable_dht,
                enable_trackers: !config.disable_trackers,
            },
            Arc::clone(&cache),
        )
        .await
        .context("cannot start the BitTorrent engine")?,
    );

    let engine_status = engine.status();
    let engine_port: Arc<dyn Engine> = engine.clone();
    let torrents = Arc::new(
        TorrentCoordinator::new(engine_port, Arc::clone(&cache), Arc::clone(&state))
            .with_trackers_file(config.data_dir.join("trackers.txt"))
            .with_read_only(config.read_only),
    );
    if let Some(dir) = &config.torrents_dir {
        tokio::spawn(Arc::clone(&torrents).watch_torrents_dir(dir.clone(), Duration::from_secs(1)));
    }
    #[cfg(feature = "r5-test-control")]
    start_test_control(Arc::clone(&torrents)).await?;
    info!(
        %address,
        data_dir = %config.data_dir.display(),
        schema_version,
        cache = ?config.cache,
        cache_size = config.cache_size,
        dht = engine_status.dht_enabled,
        peer_port = ?engine_status.listen_port,
        "listening"
    );

    let outbound = outbound_client()?;
    let search: Arc<dyn Search> = Arc::new(SearchService::new(
        RutorDatabase::new(config.data_dir.join("rutor.ls"), RUTOR_URL),
        outbound.clone(),
    ));
    search
        .set_rutor_enabled(torrents.settings().enable_rutor_search)
        .await;
    let stored = torrents.settings();
    let core: Arc<dyn ClientCore> = torrents;
    let discovery = Arc::new(DiscoveryService::new(
        Arc::clone(&core),
        address.ip(),
        address.port(),
        VERSION.into(),
        outbound.clone(),
        started,
    ));
    discovery.start(&stored).await;
    let fuse = mount_fuse(config.fuse_path.as_deref(), &core)?;
    let integrations = Integrations {
        search,
        msx: Arc::new(Msx::new(outbound, &config.data_dir)),
        discovery: Arc::clone(&discovery) as Arc<dyn rustorr_http::Discovery>,
    };
    let outcome = serve_until_signalled(
        listener,
        &mut signals,
        config.shutdown_grace,
        core,
        integrations,
        http,
    )
    .await;

    // Reverse order of startup, whatever ended the server.
    unmount_fuse(fuse);
    discovery.stop().await;
    info!("discovery stopped");
    engine.shutdown().await;
    info!("engine stopped");
    drop(cache);
    drop(state);
    info!("state closed");
    outcome
}

/// The benchmark image opts into this Unix-socket control plane. It is not
/// compiled into the production image and cannot become an HTTP contract.
#[cfg(feature = "r5-test-control")]
async fn start_test_control(torrents: Arc<TorrentCoordinator>) -> anyhow::Result<()> {
    use std::path::PathBuf;

    use rustorr_domain::InfoHash;
    use tokio::{
        io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
        net::UnixListener,
    };

    let Some(path) = std::env::var_os("RUSTORR_TEST_CONTROL_SOCKET").map(PathBuf::from) else {
        return Ok(());
    };
    if path.exists() {
        std::fs::remove_file(&path)
            .with_context(|| format!("cannot remove old control socket {}", path.display()))?;
    }
    let listener = UnixListener::bind(&path)
        .with_context(|| format!("cannot bind test control socket {}", path.display()))?;
    info!(path = %path.display(), "R5 test control ready");
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let torrents = Arc::clone(&torrents);
            tokio::spawn(async move {
                let (read, mut write) = stream.into_split();
                let mut lines = BufReader::new(read).lines();
                let response = match lines.next_line().await {
                    Ok(Some(command)) => match command.strip_prefix("evict ") {
                        Some(hash) => match hash.parse::<InfoHash>() {
                            Ok(hash) => match torrents.evict(hash).await {
                                Ok(freed) => format!("ok {freed}\n"),
                                Err(error) => format!("error {error}\n"),
                            },
                            Err(error) => format!("error {error}\n"),
                        },
                        None => "error expected `evict <infohash>`\n".into(),
                    },
                    Ok(None) => "error empty command\n".into(),
                    Err(error) => format!("error {error}\n"),
                };
                let _ = write.write_all(response.as_bytes()).await;
            });
        }
    });
    Ok(())
}

/// The state API is synchronous, so it is opened off the async threads.
async fn open_state(config: &Config) -> anyhow::Result<State> {
    let path = config.data_dir.join("rustorr.db");
    let shown = path.display().to_string();
    tokio::task::spawn_blocking(move || State::open(path))
        .await
        .context("the database task failed")?
        .with_context(|| format!("cannot open the database {shown}"))
}

fn cache_store(config: &Config) -> Arc<dyn PieceStore> {
    match config.cache {
        CacheMode::Disk => Arc::new(DiskStore::new(config.data_dir.join("cache"))),
        CacheMode::Memory => Arc::new(MemoryStore::new()),
    }
}

#[cfg(feature = "fuse")]
type Fuse = Option<rustorr_fuse::FuseMount>;
#[cfg(not(feature = "fuse"))]
type Fuse = ();

/// `FuseAutoMount`: a failed mount stops the server, as in the reference.
#[cfg(feature = "fuse")]
fn mount_fuse(path: Option<&std::path::Path>, core: &Arc<dyn ClientCore>) -> anyhow::Result<Fuse> {
    let Some(path) = path else {
        return Ok(None);
    };
    rustorr_fuse::FuseMount::mount(Arc::clone(core), path, tokio::runtime::Handle::current())
        .map(Some)
        .with_context(|| format!("cannot mount the FUSE file system at {}", path.display()))
}

#[cfg(not(feature = "fuse"))]
fn mount_fuse(path: Option<&std::path::Path>, _core: &Arc<dyn ClientCore>) -> anyhow::Result<Fuse> {
    if path.is_some() {
        bail!("this build has no FUSE support; rebuild with the `fuse` feature");
    }
    Ok(())
}

#[cfg(feature = "fuse")]
fn unmount_fuse(fuse: Fuse) {
    if let Some(mount) = fuse
        && let Err(error) = mount.unmount()
    {
        warn!(%error, "cannot unmount the FUSE file system");
    }
}

#[cfg(not(feature = "fuse"))]
fn unmount_fuse(_fuse: Fuse) {}

/// The one client for every outbound request (Rutor, Torznab, MSX). Only
/// connecting and each read are bounded, so the MSX proxy can stream long
/// bodies; search requests add an overall timeout of their own.
fn outbound_client() -> anyhow::Result<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .read_timeout(Duration::from_secs(60))
        .build()
        .context("cannot build the outbound HTTP client")
}

async fn serve_until_signalled(
    listener: TcpListener,
    signals: &mut Signals,
    grace: Duration,
    core: Arc<dyn ClientCore>,
    integrations: Integrations,
    http: HttpConfig,
) -> anyhow::Result<()> {
    let (stop, stopped) = oneshot::channel::<()>();
    let info = ServerInfo {
        // This endpoint is used by existing clients to recognise TorrServer.
        // The R2 reference capture fixes its compatibility value, independently
        // of Rustorr's package version.
        version: VERSION.into(),
    };
    let requested = http.shutdown.clone().unwrap_or_default();
    let server =
        rustorr_http::serve_with_services(listener, info, core, integrations, http, async move {
            let _ = stopped.await;
        });
    tokio::pin!(server);

    tokio::select! {
        result = &mut server => {
            result.context("the HTTP server failed")?;
            bail!("the HTTP server stopped without being asked to")
        }
        () = requested.notified() => {
            info!("shutdown requested over HTTP");
            stop_gracefully(server.as_mut(), stop, grace).await
        }
        signal = signals.next() => {
            info!(signal, "shutdown signal received");
            stop_gracefully(server.as_mut(), stop, grace).await
        }
    }
}

async fn stop_gracefully<F>(
    server: Pin<&mut F>,
    stop: oneshot::Sender<()>,
    grace: Duration,
) -> anyhow::Result<()>
where
    F: Future<Output = io::Result<()>>,
{
    match stop_within(server, stop, grace)
        .await
        .context("the HTTP server failed while stopping")?
    {
        Stopped::Cleanly => info!("http server stopped"),
        Stopped::TimedOut => warn!(
            grace_seconds = grace.as_secs(),
            "open connections did not finish in time and were closed"
        ),
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
enum Stopped {
    Cleanly,
    TimedOut,
}

/// Tells the server to stop and waits up to `grace` for it to drain. The
/// server's own drain has no limit, and a viewer's open stream would otherwise
/// keep the process alive until the container runtime kills it.
async fn stop_within<F>(
    mut server: Pin<&mut F>,
    stop: oneshot::Sender<()>,
    grace: Duration,
) -> io::Result<Stopped>
where
    F: Future<Output = io::Result<()>>,
{
    let _ = stop.send(());
    match tokio::time::timeout(grace, server.as_mut()).await {
        Ok(result) => result.map(|()| Stopped::Cleanly),
        Err(_) => Ok(Stopped::TimedOut),
    }
}

struct Signals {
    terminate: Signal,
    interrupt: Signal,
}

impl Signals {
    fn install() -> io::Result<Self> {
        Ok(Self {
            terminate: signal(SignalKind::terminate())?,
            interrupt: signal(SignalKind::interrupt())?,
        })
    }

    async fn next(&mut self) -> &'static str {
        tokio::select! {
            _ = self.terminate.recv() => "SIGTERM",
            _ = self.interrupt.recv() => "SIGINT",
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    #[tokio::test]
    async fn a_server_that_drains_in_time_stops_cleanly() {
        let (stop, stopped) = oneshot::channel::<()>();
        let server = async move {
            let _ = stopped.await;
            Ok::<(), io::Error>(())
        };
        tokio::pin!(server);

        let outcome = stop_within(server.as_mut(), stop, Duration::from_secs(5)).await;

        assert_eq!(outcome.unwrap(), Stopped::Cleanly);
    }

    #[tokio::test]
    async fn a_connection_that_never_finishes_is_cut_off_after_the_grace_period() {
        let (stop, _stopped) = oneshot::channel::<()>();
        let server = std::future::pending::<io::Result<()>>();
        tokio::pin!(server);

        let started = Instant::now();
        let outcome = stop_within(server.as_mut(), stop, Duration::from_millis(150)).await;

        assert_eq!(outcome.unwrap(), Stopped::TimedOut);
        let waited = started.elapsed();
        assert!(waited >= Duration::from_millis(150), "{waited:?}");
        assert!(waited < Duration::from_secs(3), "{waited:?}");
    }

    #[tokio::test]
    async fn a_server_that_fails_while_stopping_reports_the_error() {
        let (stop, _stopped) = oneshot::channel::<()>();
        let server = async { Err::<(), _>(io::Error::other("accept loop died")) };
        tokio::pin!(server);

        let outcome = stop_within(server.as_mut(), stop, Duration::from_secs(5)).await;

        assert_eq!(outcome.unwrap_err().to_string(), "accept loop died");
    }

    #[tokio::test]
    async fn the_stop_request_reaches_the_server() {
        let (stop, stopped) = oneshot::channel::<()>();
        let (seen, was_seen) = oneshot::channel::<()>();
        let server = async move {
            let _ = stopped.await;
            let _ = seen.send(());
            Ok::<(), io::Error>(())
        };
        tokio::pin!(server);

        stop_within(server.as_mut(), stop, Duration::from_secs(5))
            .await
            .unwrap();

        assert!(
            was_seen.await.is_ok(),
            "the server never saw the stop request"
        );
    }
}
