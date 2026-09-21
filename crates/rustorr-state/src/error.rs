use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error("cannot create directory {}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("database error while {context}")]
    Database {
        context: &'static str,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("database has schema version {found}, this build understands up to {supported}")]
    SchemaTooNew { found: u32, supported: u32 },
    #[error("the file is not a Rustorr database")]
    NotRustorrDatabase,
    #[error("invalid {field}: {reason}")]
    InvalidValue {
        field: &'static str,
        reason: &'static str,
    },
}

/// Wraps a SQLite error so that it does not leak into other crates' types.
pub(crate) fn database(context: &'static str) -> impl FnOnce(rusqlite::Error) -> Error {
    move |source| Error::Database {
        context,
        source: Box::new(source),
    }
}
