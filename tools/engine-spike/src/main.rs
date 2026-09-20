mod storage_probe;

use std::{
    collections::BTreeSet,
    env, fs,
    io::SeekFrom,
    net::{SocketAddr, ToSocketAddrs},
    num::NonZeroU32,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use librqbit::{
    AddTorrent, AddTorrentOptions, ConnectionOptions, DhtSessionConfig, ListenerMode,
    ListenerOptions, Session, SessionOptions, SessionPersistenceConfig,
    http_api_types::PeerStatsFilter, limits::LimitsConfig, storage::StorageFactoryExt,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use storage_probe::RecordingStorageFactory;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

const SCHEMA: &str = "rustorr.r3.engine-spike.v1";

#[derive(Debug, Clone)]
struct Args {
    torrent: Option<PathBuf>,
    magnet: Option<String>,
    output: PathBuf,
    file_id: usize,
    offset: u64,
    length: usize,
    views: usize,
    read_timeout_ms: u64,
    expected_sha256: Option<String>,
    cancel_after_ms: Option<u64>,
    initialize_timeout_ms: u64,
    persistence: Option<PathBuf>,
    disable_dht: bool,
    dht_bootstrap: Option<Vec<String>>,
    dht_port: Option<u16>,
    enable_utp_listener: bool,
    utp_only: bool,
    listen_port: Option<u16>,
    initial_peers: Option<Vec<String>>,
    disable_trackers: bool,
    wait_complete_ms: Option<u64>,
    pex_observe_ms: u64,
    upload_limit_bps: Option<u32>,
    download_limit_bps: Option<u32>,
    evict_after_read: bool,
    warmup_offset: Option<u64>,
    result_file: Option<PathBuf>,
    hold_ms: u64,
    tracker: Option<String>,
    only_files: Option<Vec<usize>>,
    custom_storage: bool,
    delete_and_refetch: bool,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            torrent: None,
            magnet: None,
            output: PathBuf::from("/tmp/rustorr-engine-spike/state"),
            file_id: 0,
            offset: 0,
            length: 256 * 1024,
            views: 1,
            read_timeout_ms: 60_000,
            expected_sha256: None,
            cancel_after_ms: None,
            initialize_timeout_ms: 60_000,
            persistence: None,
            disable_dht: false,
            dht_bootstrap: None,
            dht_port: None,
            enable_utp_listener: false,
            utp_only: false,
            listen_port: None,
            initial_peers: None,
            disable_trackers: false,
            wait_complete_ms: None,
            pex_observe_ms: 0,
            upload_limit_bps: None,
            download_limit_bps: None,
            evict_after_read: false,
            warmup_offset: None,
            result_file: None,
            hold_ms: 0,
            tracker: None,
            only_files: None,
            custom_storage: false,
            delete_and_refetch: false,
        }
    }
}

fn usage() -> &'static str {
    "usage: rustorr-engine-spike (--torrent PATH | --magnet URL) [options]\n\n\
options:\n\
  --output PATH                 torrent output directory\n\
  --file-id N                   file index to stream (default: 0)\n\
  --offset N                    byte offset to seek to (default: 0)\n\
  --length N                    bytes to read (default: 262144)\n\
  --views N                     concurrent reads in one session (default: 1)\n\
  --read-timeout-ms N           fail if a read exceeds N ms (default: 60000)\n\
  --expected-sha256 HEX         expected digest for the returned bytes\n\
  --cancel-after-ms N           cancel the read after N milliseconds\n\
  --initialize-timeout-ms N     fail if initialization exceeds N ms (default: 60000)\n\
  --persistence PATH            enable session persistence in PATH\n\
  --disable-dht                 disable DHT for tracker-only probes\n\
  --dht-bootstrap HOST:PORT,... use isolated DHT bootstrap nodes\n\
  --dht-port N                  bind DHT to a fixed UDP port\n\
  --enable-utp-listener         bind TCP and uTP listeners for capability probes\n\
  --utp-only                    use uTP only for peer connections\n\
  --listen-port N               bind the peer listener to a fixed port\n\
  --initial-peers HOST:PORT,... seed the peer set without tracker or DHT\n\
  --disable-trackers            ignore every tracker, including metadata ones\n\
  --wait-complete-ms N          wait until the torrent completes before holding\n\
  --pex-observe-ms N            poll peer stats for N ms and report new peers\n\
  --upload-limit-bps N          throttle this session's upload rate\n\
  --download-limit-bps N        throttle this session's download rate\n\
  --evict-after-read            zero the read range on disk and read it again\n\
  --warmup-offset N             read this offset first, then time the real read\n\
  --result-file PATH            also write the result JSON to PATH\n\
  --hold-ms N                   keep the session alive after emitting JSON\n\
  --tracker URL                 add a tracker override\n\
  --only-files N,...            select file IDs\n\
  --custom-storage              use the recording StorageFactory probe\n\
  --delete-and-refetch           delete files, re-add torrent and read again\n\
  --help                        print this help"
}

fn parse_value<I>(args: &mut I, flag: &str) -> Result<String>
where
    I: Iterator<Item = String>,
{
    args.next()
        .with_context(|| format!("missing value for {flag}"))
}

fn parse_list(value: &str) -> Result<Vec<usize>> {
    value
        .split(',')
        .map(|item| {
            item.parse()
                .with_context(|| format!("invalid list item: {item}"))
        })
        .collect()
}

fn parse_args() -> Result<Args> {
    let mut parsed = Args::default();
    let mut args = env::args().skip(1);
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--torrent" => parsed.torrent = Some(parse_value(&mut args, &flag)?.into()),
            "--magnet" => parsed.magnet = Some(parse_value(&mut args, &flag)?),
            "--output" => parsed.output = parse_value(&mut args, &flag)?.into(),
            "--file-id" => parsed.file_id = parse_value(&mut args, &flag)?.parse()?,
            "--offset" => parsed.offset = parse_value(&mut args, &flag)?.parse()?,
            "--length" => parsed.length = parse_value(&mut args, &flag)?.parse()?,
            "--views" => {
                parsed.views = parse_value(&mut args, &flag)?.parse()?;
                if parsed.views == 0 {
                    bail!("--views must be greater than zero")
                }
            }
            "--read-timeout-ms" => {
                parsed.read_timeout_ms = parse_value(&mut args, &flag)?.parse()?
            }
            "--expected-sha256" => parsed.expected_sha256 = Some(parse_value(&mut args, &flag)?),
            "--cancel-after-ms" => {
                parsed.cancel_after_ms = Some(parse_value(&mut args, &flag)?.parse()?)
            }
            "--initialize-timeout-ms" => {
                parsed.initialize_timeout_ms = parse_value(&mut args, &flag)?.parse()?
            }
            "--persistence" => parsed.persistence = Some(parse_value(&mut args, &flag)?.into()),
            "--disable-dht" => parsed.disable_dht = true,
            "--dht-bootstrap" => {
                parsed.dht_bootstrap = Some(
                    parse_value(&mut args, &flag)?
                        .split(',')
                        .map(ToOwned::to_owned)
                        .collect(),
                )
            }
            "--dht-port" => parsed.dht_port = Some(parse_value(&mut args, &flag)?.parse()?),
            "--enable-utp-listener" => parsed.enable_utp_listener = true,
            "--utp-only" => parsed.utp_only = true,
            "--listen-port" => parsed.listen_port = Some(parse_value(&mut args, &flag)?.parse()?),
            "--initial-peers" => {
                parsed.initial_peers = Some(
                    parse_value(&mut args, &flag)?
                        .split(',')
                        .map(ToOwned::to_owned)
                        .collect(),
                )
            }
            "--disable-trackers" => parsed.disable_trackers = true,
            "--wait-complete-ms" => {
                parsed.wait_complete_ms = Some(parse_value(&mut args, &flag)?.parse()?)
            }
            "--pex-observe-ms" => parsed.pex_observe_ms = parse_value(&mut args, &flag)?.parse()?,
            "--upload-limit-bps" => {
                parsed.upload_limit_bps = Some(parse_value(&mut args, &flag)?.parse()?)
            }
            "--download-limit-bps" => {
                parsed.download_limit_bps = Some(parse_value(&mut args, &flag)?.parse()?)
            }
            "--evict-after-read" => parsed.evict_after_read = true,
            "--warmup-offset" => {
                parsed.warmup_offset = Some(parse_value(&mut args, &flag)?.parse()?)
            }
            "--result-file" => parsed.result_file = Some(parse_value(&mut args, &flag)?.into()),
            "--hold-ms" => parsed.hold_ms = parse_value(&mut args, &flag)?.parse()?,
            "--tracker" => parsed.tracker = Some(parse_value(&mut args, &flag)?),
            "--only-files" => {
                parsed.only_files = Some(parse_list(&parse_value(&mut args, &flag)?)?)
            }
            "--custom-storage" => parsed.custom_storage = true,
            "--delete-and-refetch" => parsed.delete_and_refetch = true,
            "--help" => {
                println!("{}", usage());
                std::process::exit(0);
            }
            other => bail!("unknown argument {other}\n\n{}", usage()),
        }
    }

    if parsed.torrent.is_some() == parsed.magnet.is_some() {
        bail!(
            "exactly one of --torrent or --magnet is required\n\n{}",
            usage()
        )
    }
    Ok(parsed)
}

fn torrent_source(args: &Args) -> Result<AddTorrent<'static>> {
    if let Some(path) = &args.torrent {
        return Ok(AddTorrent::from_bytes(fs::read(path).with_context(
            || format!("failed to read torrent metadata from {}", path.display()),
        )?));
    }
    Ok(AddTorrent::from_url(
        args.magnet.clone().expect("validated magnet"),
    ))
}

fn files_value(handle: &Arc<librqbit::ManagedTorrent>) -> Result<Value> {
    handle
        .with_metadata(|metadata| {
            let files: Vec<Value> = metadata
                .info
                .iter_file_details()
                .enumerate()
                .map(|(index, details)| {
                    json!({
                        "index": index,
                        "name": details.filename.to_string(),
                        "components": details.filename.to_vec(),
                        "length": details.len,
                    })
                })
                .collect();
            json!({
                "name": metadata.info.name().map(|name| name.into_owned()),
                "total_pieces": metadata.info.lengths().total_pieces(),
                "files": files,
            })
        })
        .context("metadata was not available after initialization")
}

fn resolve_peers(values: &[String]) -> Result<Vec<SocketAddr>> {
    let mut resolved = Vec::new();
    for value in values {
        let addrs: Vec<SocketAddr> = value
            .to_socket_addrs()
            .with_context(|| format!("failed to resolve initial peer {value}"))?
            .filter(|addr| addr.is_ipv4())
            .collect();
        if addrs.is_empty() {
            bail!("initial peer {value} resolved to no IPv4 address")
        }
        resolved.extend(addrs);
    }
    Ok(resolved)
}

fn peer_snapshot(handle: &Arc<librqbit::ManagedTorrent>) -> Result<Value> {
    let Some(live) = handle.live() else {
        return Ok(Value::Null);
    };
    let filter: PeerStatsFilter = serde_json::from_value(json!({"state": "all"}))
        .context("failed to build the all-states peer stats filter")?;
    serde_json::to_value(live.per_peer_stats_snapshot(filter))
        .context("failed to serialize the peer stats snapshot")
}

fn snapshot_addrs(snapshot: &Value) -> BTreeSet<String> {
    snapshot
        .get("peers")
        .and_then(Value::as_object)
        .map(|peers| peers.keys().cloned().collect())
        .unwrap_or_default()
}

/// Simulates a Rustorr-owned cache eviction underneath a live torrent: the
/// bytes are zeroed on disk while the engine still believes the piece is
/// present. The second read shows what the engine does with lost storage.
async fn evict_and_reread(
    handle: &Arc<librqbit::ManagedTorrent>,
    args: &Args,
    before: Option<&str>,
) -> Result<Value> {
    let relative = handle
        .with_metadata(|metadata| {
            metadata
                .info
                .iter_file_details()
                .nth(args.file_id)
                .map(|details| details.filename.to_vec().join("/"))
        })
        .context("metadata was not available for the eviction probe")?
        .with_context(|| format!("file {} is not part of the torrent", args.file_id))?;
    let path = args.output.join(&relative);

    let mut file = fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .with_context(|| format!("failed to open {} for eviction", path.display()))?;
    {
        use std::io::{Seek, Write};
        file.seek(SeekFrom::Start(args.offset))
            .context("failed to seek the evicted range")?;
        file.write_all(&vec![0u8; args.length])
            .context("failed to zero the evicted range")?;
        file.flush().context("failed to flush the evicted range")?;
    }
    eprintln!(
        "phase=evicted path={} offset={}",
        path.display(),
        args.offset
    );

    let mut reread = Args::clone(args);
    reread.expected_sha256 = None;
    let (value, after) = read_probe(handle, &reread).await?;
    Ok(json!({
        "evicted_path": path.display().to_string(),
        "evicted_offset": args.offset,
        "evicted_bytes": args.length,
        "sha256_before": before,
        "reread": value,
        "recovered": match (before, after.as_deref()) {
            (Some(before), Some(after)) => Some(before.eq_ignore_ascii_case(after)),
            _ => None,
        },
    }))
}

/// Watches the peer set of a tracker-hidden, DHT-disabled session. Any address
/// that is not one of the configured initial peers can only have arrived over
/// peer exchange, so the discovery timestamp is the observable PEX outcome.
async fn observe_peer_exchange(
    handle: &Arc<librqbit::ManagedTorrent>,
    args: &Args,
    initial: &[SocketAddr],
) -> Result<Value> {
    let known: BTreeSet<String> = initial.iter().map(ToString::to_string).collect();
    let started = Instant::now();
    let mut first_discovery_ms: Option<f64> = None;
    let mut snapshot;

    loop {
        snapshot = peer_snapshot(handle)?;
        let discovered: BTreeSet<String> = snapshot_addrs(&snapshot)
            .difference(&known)
            .cloned()
            .collect();
        if first_discovery_ms.is_none() && !discovered.is_empty() {
            first_discovery_ms = Some(started.elapsed().as_secs_f64() * 1000.0);
            eprintln!("phase=pex_discovered peers={discovered:?}");
        }
        if started.elapsed() >= Duration::from_millis(args.pex_observe_ms) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    let discovered_addrs: BTreeSet<String> = snapshot_addrs(&snapshot)
        .difference(&known)
        .cloned()
        .collect();
    let discovered: Vec<Value> = discovered_addrs
        .iter()
        .map(|addr| {
            let peer = snapshot.pointer(&format!("/peers/{addr}")).cloned();
            json!({
                "addr": addr,
                "state": peer.as_ref().and_then(|p| p.get("state").cloned()),
                "client_name": peer.as_ref().and_then(|p| p.get("client_name").cloned()),
                "incoming_connections": peer
                    .as_ref()
                    .and_then(|p| p.pointer("/counters/incoming_connections").cloned()),
                "fetched_bytes": peer
                    .as_ref()
                    .and_then(|p| p.pointer("/counters/fetched_bytes").cloned()),
            })
        })
        .collect();

    Ok(json!({
        "observe_ms": args.pex_observe_ms,
        "trackers_disabled": args.disable_trackers,
        "dht_disabled": args.disable_dht,
        "initial_peers": known,
        "first_discovery_ms": first_discovery_ms,
        "discovered_peer_count": discovered.len(),
        "discovered_peers": discovered,
        "snapshot": snapshot,
    }))
}

async fn read_probe(
    handle: &Arc<librqbit::ManagedTorrent>,
    args: &Args,
) -> Result<(Value, Option<String>)> {
    let started = Instant::now();
    let mut stream = handle
        .clone()
        .stream(args.file_id)
        .await
        .with_context(|| format!("failed to open file stream {}", args.file_id))?;
    stream
        .seek(SeekFrom::Start(args.offset))
        .await
        .context("failed to seek stream")?;

    let mut bytes = vec![0; args.length];
    let read_result = if let Some(timeout_ms) = args.cancel_after_ms {
        match tokio::time::timeout(
            Duration::from_millis(timeout_ms),
            stream.read_exact(&mut bytes),
        )
        .await
        {
            Ok(result) => Some(result.context("stream read failed")?),
            Err(_) => None,
        }
    } else {
        Some(
            tokio::time::timeout(
                Duration::from_millis(args.read_timeout_ms),
                stream.read_exact(&mut bytes),
            )
            .await
            .context("stream read timed out")?
            .context("stream read failed")?,
        )
    };

    let Some(bytes_read) = read_result else {
        return Ok((
            json!({
                "cancelled": true,
                "duration_ms": started.elapsed().as_secs_f64() * 1000.0,
                "offset": args.offset,
                "requested_bytes": args.length,
            }),
            None,
        ));
    };

    let digest = hex::encode(Sha256::digest(&bytes));
    let expected_match = args
        .expected_sha256
        .as_ref()
        .map(|expected| expected.eq_ignore_ascii_case(&digest));
    if expected_match == Some(false) {
        bail!("returned byte digest does not match --expected-sha256: {digest}")
    }

    Ok((
        json!({
            "cancelled": false,
            "duration_ms": started.elapsed().as_secs_f64() * 1000.0,
            "offset": args.offset,
            "requested_bytes": args.length,
            "bytes_read": bytes_read,
            "sha256": digest,
            "expected_sha256_match": expected_match,
        }),
        Some(digest),
    ))
}

async fn read_views(handle: &Arc<librqbit::ManagedTorrent>, args: &Args) -> Result<Value> {
    if args.views == 1 {
        return Ok(read_probe(handle, args).await?.0);
    }

    let mut tasks = Vec::with_capacity(args.views);
    for _ in 0..args.views {
        let handle = handle.clone();
        let args = args.clone();
        tasks.push(tokio::spawn(async move {
            read_probe(&handle, &args).await.map(|result| result.0)
        }));
    }

    let mut reads = Vec::with_capacity(args.views);
    for task in tasks {
        reads.push(task.await.context("concurrent read task failed")??);
    }
    Ok(json!({"views": reads}))
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = parse_args()?;
    fs::create_dir_all(&args.output).with_context(|| {
        format!(
            "failed to create output directory {}",
            args.output.display()
        )
    })?;

    // librqbit 9.0.1 accepts AddTorrentOptions::disable_trackers but never reads
    // it; only the session-level flag clears the tracker list, so set it here.
    let mut session_options = SessionOptions {
        disable_trackers: args.disable_trackers,
        dht: if args.disable_dht {
            None
        } else {
            Some(DhtSessionConfig {
                bootstrap_addrs: args.dht_bootstrap.clone(),
                port: args.dht_port,
                persistence: None,
            })
        },
        disable_local_service_discovery: true,
        ipv4_only: true,
        ..Default::default()
    };
    if args.enable_utp_listener || args.utp_only || args.listen_port.is_some() {
        session_options.listen = Some(ListenerOptions {
            mode: if args.utp_only {
                ListenerMode::UtpOnly
            } else if args.enable_utp_listener {
                ListenerMode::TcpAndUtp
            } else {
                ListenerMode::TcpOnly
            },
            listen_addr: SocketAddr::from(([0, 0, 0, 0], args.listen_port.unwrap_or(0))),
            ipv4_only: true,
            ..Default::default()
        });
    }
    if args.utp_only {
        session_options.connect = Some(ConnectionOptions {
            enable_tcp: false,
            ..Default::default()
        });
    }
    if let Some(path) = &args.persistence {
        session_options.persistence = Some(SessionPersistenceConfig::Json {
            folder: Some(path.clone()),
        });
        session_options.fastresume = true;
    }

    let initial_peers = match &args.initial_peers {
        Some(values) => resolve_peers(values)?,
        None => Vec::new(),
    };

    let session = Session::new_with_opts(args.output.clone(), session_options).await?;
    let counters = RecordingStorageFactory::default();
    let mut add_options = AddTorrentOptions {
        output_folder: Some(args.output.to_string_lossy().into_owned()),
        overwrite: true,
        only_files: args.only_files.clone(),
        initial_peers: (!initial_peers.is_empty()).then(|| initial_peers.clone()),
        ratelimits: LimitsConfig {
            upload_bps: args.upload_limit_bps.and_then(NonZeroU32::new),
            download_bps: args.download_limit_bps.and_then(NonZeroU32::new),
        },
        ..Default::default()
    };
    if let Some(tracker) = &args.tracker {
        add_options.trackers = Some(vec![tracker.clone()]);
    }
    if args.custom_storage {
        add_options.storage_factory = Some(Box::new(counters.clone()).boxed());
    }

    let source = torrent_source(&args)?;
    let initialized_at = Instant::now();
    let response = session
        .add_torrent(source, Some(add_options))
        .await
        .context("failed to add torrent")?;
    eprintln!("phase=added");
    let handle = response
        .into_handle()
        .context("add returned no managed torrent handle")?;
    tokio::time::timeout(
        Duration::from_millis(args.initialize_timeout_ms),
        handle.wait_until_initialized(),
    )
    .await
    .context("torrent initialization timed out")?
    .context("torrent did not initialize")?;
    let initialized_ms = initialized_at.elapsed().as_secs_f64() * 1000.0;
    eprintln!("phase=initialized ms={initialized_ms}");
    eprintln!(
        "stats_after_initialization={}",
        serde_json::to_string(&handle.stats())?
    );

    let metadata = files_value(&handle)?;
    // R1 times its seek only after a first range has already been served, so
    // an unmeasured warm-up read is what makes the two numbers comparable.
    let warmup = match args.warmup_offset {
        Some(offset) => {
            let mut warmup_args = Args::clone(&args);
            warmup_args.offset = offset;
            warmup_args.expected_sha256 = None;
            let (value, _) = read_probe(&handle, &warmup_args).await?;
            eprintln!("phase=warmup");
            Some(value)
        }
        None => None,
    };
    let read = read_views(&handle, &args).await?;
    eprintln!("phase=read");
    let read_digest = read
        .get("sha256")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let eviction = if args.evict_after_read {
        Some(evict_and_reread(&handle, &args, read_digest.as_deref()).await?)
    } else {
        None
    };
    let peer_exchange = if args.pex_observe_ms > 0 {
        let observed = observe_peer_exchange(&handle, &args, &initial_peers).await?;
        eprintln!("phase=pex_observed");
        Some(observed)
    } else {
        None
    };
    let completed = match args.wait_complete_ms {
        Some(timeout_ms) => {
            let waited = Instant::now();
            let outcome = tokio::time::timeout(
                Duration::from_millis(timeout_ms),
                handle.wait_until_completed(),
            )
            .await;
            eprintln!("phase=completed ok={}", outcome.is_ok());
            Some(json!({
                "timeout_ms": timeout_ms,
                "completed": outcome.is_ok(),
                "duration_ms": waited.elapsed().as_secs_f64() * 1000.0,
            }))
        }
        None => None,
    };
    let refetch = if args.delete_and_refetch {
        let torrent_id = handle.id();
        session
            .delete(torrent_id.into(), true)
            .await
            .context("failed to delete torrent before re-fetch")?;
        eprintln!("phase=deleted");

        let mut refetch_options = AddTorrentOptions {
            output_folder: Some(args.output.to_string_lossy().into_owned()),
            overwrite: true,
            only_files: args.only_files.clone(),
            ..Default::default()
        };
        if let Some(tracker) = &args.tracker {
            refetch_options.trackers = Some(vec![tracker.clone()]);
        }
        if args.custom_storage {
            refetch_options.storage_factory = Some(Box::new(counters.clone()).boxed());
        }

        let refetch_response = session
            .add_torrent(torrent_source(&args)?, Some(refetch_options))
            .await
            .context("failed to re-add torrent after delete")?;
        eprintln!("phase=readded");
        let refetch_handle = refetch_response
            .into_handle()
            .context("re-add returned no managed torrent handle")?;
        tokio::time::timeout(
            Duration::from_millis(args.initialize_timeout_ms),
            refetch_handle.wait_until_initialized(),
        )
        .await
        .context("re-fetch initialization timed out")?
        .context("re-fetch torrent did not initialize")?;
        eprintln!("phase=refetch_initialized");
        let refetch_read = read_views(&refetch_handle, &args).await?;
        eprintln!("phase=refetch_read");
        Some(json!({
            "deleted_files": true,
            "read": refetch_read,
            "stats": serde_json::to_value(refetch_handle.stats())?,
        }))
    } else {
        None
    };
    let capability_state = json!({
        "dht": {
            "enabled": session.get_dht().is_some(),
            "stats": session
                .get_dht()
                .map(|dht| serde_json::to_value(dht.stats()))
                .transpose()?,
        },
        "listener": {
            "utp_requested": args.enable_utp_listener || args.utp_only,
            "utp_only": args.utp_only,
            "bound_addr": session.listen_addr().map(|addr| addr.to_string()),
            "announce_port": session.announce_port(),
        },
    });
    let stats =
        serde_json::to_value(handle.stats()).context("failed to serialize torrent stats")?;
    let result = json!({
        "schema": SCHEMA,
        "engine": {
            "name": "librqbit",
            "version": librqbit::version(),
            "dependency": "librqbit = 9.0.1",
        },
        "source": {
            "kind": if args.torrent.is_some() { "torrent" } else { "magnet" },
            "torrent": args.torrent.as_ref().map(|path| path.display().to_string()),
        },
        "probe": {
            "initialized_ms": initialized_ms,
            "total_ms": initialized_at.elapsed().as_secs_f64() * 1000.0,
            "metadata": metadata,
            "warmup_read": warmup,
            "read": read,
            "eviction": eviction,
            "peer_exchange": peer_exchange,
            "completed": completed,
            "delete_and_refetch": refetch,
            "stats": stats,
            "custom_storage": args.custom_storage,
            "storage_counters": counters.counters.snapshot(),
            "persistence": args.persistence.as_ref().map(|path| path.display().to_string()),
            "views": args.views,
            "dht_disabled": args.disable_dht,
            "trackers_disabled": args.disable_trackers,
            "initial_peers": initial_peers
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            "peer_snapshot": peer_snapshot(&handle)?,
            "capabilities": capability_state,
            "hold_ms": args.hold_ms,
            "initialize_timeout_ms": args.initialize_timeout_ms,
            "read_timeout_ms": args.read_timeout_ms,
        },
    });

    let rendered = serde_json::to_string_pretty(&result)?;
    println!("{rendered}");
    if let Some(path) = &args.result_file {
        fs::write(path, format!("{rendered}\n"))
            .with_context(|| format!("failed to write result file {}", path.display()))?;
    }
    if args.hold_ms > 0 {
        tokio::time::sleep(Duration::from_millis(args.hold_ms)).await;
    }
    session.stop().await;
    Ok(())
}
