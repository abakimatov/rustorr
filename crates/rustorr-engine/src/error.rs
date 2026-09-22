use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error("cannot create engine directory {}", path.display())]
    DataDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("engine session failed to start")]
    Start(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("engine did not apply {setting}: expected {expected}, got {actual}")]
    SettingNotApplied {
        setting: &'static str,
        expected: String,
        actual: String,
    },
    #[error("torrent operation failed: {0}")]
    Torrent(#[source] anyhow::Error),
    #[error("torrent {0} is not active in the engine")]
    NotLoaded(rustorr_domain::InfoHash),
}
