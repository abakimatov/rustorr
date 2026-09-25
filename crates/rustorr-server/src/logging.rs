use std::{
    fs::{self, OpenOptions},
    io::{self, IsTerminal},
    path::Path,
    sync::Mutex,
};

use tracing_subscriber::{EnvFilter, fmt::writer::BoxMakeWriter};

use crate::config::LogFormat;

/// MatriX.145 starts a log file afresh once it reaches 100 MB.
const LOG_FILE_LIMIT: u64 = 100 * 1024 * 1024;

/// Logs go to stderr, or to `file` (`--log-file`), filtered by `RUST_LOG` and
/// `info` by default. An invalid `RUST_LOG` falls back to the default instead
/// of hiding the server's output.
pub fn init(format: LogFormat, file: Option<&Path>) -> io::Result<()> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let (writer, ansi) = match file {
        Some(path) => (BoxMakeWriter::new(Mutex::new(open_log_file(path)?)), false),
        None => (
            BoxMakeWriter::new(std::io::stderr),
            std::io::stderr().is_terminal(),
        ),
    };
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(writer)
        .with_ansi(ansi);
    match format {
        LogFormat::Text => builder.init(),
        LogFormat::Json => builder.json().init(),
    }
    Ok(())
}

/// `openLogFile`: append, after removing a file that grew to the limit.
pub fn open_log_file(path: &Path) -> io::Result<fs::File> {
    if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.len() >= LOG_FILE_LIMIT) {
        let _ = fs::remove_file(path);
    }
    OpenOptions::new().create(true).append(true).open(path)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    #[test]
    fn a_full_log_file_is_started_afresh() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server.log");
        fs::write(&path, b"old\n").unwrap();
        open_log_file(&path).unwrap().write_all(b"new\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "old\nnew\n");

        fs::File::create(&path)
            .unwrap()
            .set_len(LOG_FILE_LIMIT)
            .unwrap();
        open_log_file(&path).unwrap().write_all(b"fresh\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "fresh\n");
    }
}
