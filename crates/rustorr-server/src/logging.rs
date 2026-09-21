use std::io::IsTerminal;

use tracing_subscriber::EnvFilter;

use crate::config::LogFormat;

/// Logs go to stderr, filtered by `RUST_LOG` and `info` by default. An invalid
/// `RUST_LOG` falls back to the default instead of hiding the server's output.
pub fn init(format: LogFormat) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal());
    match format {
        LogFormat::Text => builder.init(),
        LogFormat::Json => builder.json().init(),
    }
}
