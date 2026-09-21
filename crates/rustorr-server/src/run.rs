//! The composition root: the one place that builds the concrete engine, cache,
//! state and HTTP server, starts them in order and stops them in reverse.

use std::{future::Future, io, pin::Pin, sync::Arc, time::Duration};

use anyhow::{Context, bail};
use rustorr_cache::{Cache, CacheConfig, DiskStore, MemoryStore, PieceStore};
use rustorr_engine::{Engine, EngineConfig, LibrqbitEngine};
use rustorr_http::ServerInfo;
use rustorr_state::State;
use tokio::{
    net::TcpListener,
    signal::unix::{Signal, SignalKind, signal},
    sync::oneshot,
};
use tracing::{info, warn};

use crate::config::{CacheMode, Config};

pub async fn run(config: Config) -> anyhow::Result<()> {
    // Installed first so a signal that arrives during startup is not lost.
    let mut signals = Signals::install().context("cannot install signal handlers")?;

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
    let state = open_state(&config).await?;
    let schema_version = state
        .schema_version()
        .context("cannot read the schema version")?;
    let cache = Arc::new(Cache::new(
        cache_store(&config),
        CacheConfig {
            cap_bytes: config.cache_size,
        },
    ));
    let engine = LibrqbitEngine::start(EngineConfig {
        data_dir: config.data_dir.join("engine"),
        listen_port: Some(config.peer_port),
        enable_dht: !config.disable_dht,
        enable_trackers: !config.disable_trackers,
    })
    .await
    .context("cannot start the BitTorrent engine")?;

    let engine_status = engine.status();
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

    let outcome = serve_until_signalled(listener, &mut signals, config.shutdown_grace).await;

    // Reverse order of startup, whatever ended the server.
    engine.shutdown().await;
    info!("engine stopped");
    drop(cache);
    drop(state);
    info!("state closed");
    outcome
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

async fn serve_until_signalled(
    listener: TcpListener,
    signals: &mut Signals,
    grace: Duration,
) -> anyhow::Result<()> {
    let (stop, stopped) = oneshot::channel::<()>();
    let info = ServerInfo {
        // This endpoint is used by existing clients to recognise TorrServer.
        // The R2 reference capture fixes its compatibility value, independently
        // of Rustorr's package version.
        version: "MatriX.145".into(),
    };
    let server = rustorr_http::serve(listener, info, async move {
        let _ = stopped.await;
    });
    tokio::pin!(server);

    tokio::select! {
        result = &mut server => {
            result.context("the HTTP server failed")?;
            bail!("the HTTP server stopped without being asked to")
        }
        signal = signals.next() => {
            info!(signal, "shutdown signal received");
            match stop_within(server.as_mut(), stop, grace)
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
    }
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
