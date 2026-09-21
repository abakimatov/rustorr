/// What a running engine reports about itself, read back after start rather
/// than copied from the configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineStatus {
    pub dht_enabled: bool,
    /// Port the engine accepts incoming peers on, if it listens at all.
    pub listen_port: Option<u16>,
}

/// The BitTorrent engine as the rest of Rustorr sees it, in domain terms.
///
/// Torrent operations are not part of the port yet; they arrive with the
/// torrent lifecycle in R5.
pub trait Engine: Send + Sync {
    fn status(&self) -> &EngineStatus;
}
