use std::{net::IpAddr, path::PathBuf};

/// Session-level settings for the BitTorrent engine.
///
/// These apply to the whole engine session. Do not try to override them per
/// torrent: librqbit 9.0.1 accepts `AddTorrentOptions::disable_trackers` but
/// never reads it (ADR 0003, condition 3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineConfig {
    /// Directory owned by the engine: the session's scratch output folder and
    /// the DHT routing-table cache. Created on start if missing.
    pub data_dir: PathBuf,
    /// TCP and uTP port for incoming peers. `Some(0)` picks a free port,
    /// `None` runs without a listener.
    pub listen_port: Option<u16>,
    /// Address the peer listener binds; `None` is every interface, IPv6 and
    /// IPv4.
    pub listen_ip: Option<IpAddr>,
    pub enable_dht: bool,
    pub enable_trackers: bool,
    /// `socks5://[user:password@]host:port` for outgoing peer connections
    /// and HTTP(S) tracker requests. librqbit 9.0.1 has one proxy for both
    /// and supports no other scheme.
    pub proxy_url: Option<String>,
}
