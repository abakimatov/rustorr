use std::path::PathBuf;

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
    pub enable_dht: bool,
    pub enable_trackers: bool,
}
