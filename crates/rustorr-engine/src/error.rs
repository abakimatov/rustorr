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
}
