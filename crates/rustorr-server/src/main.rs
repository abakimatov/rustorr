//! `rustorr` binary: configuration, composition root, observability and
//! graceful shutdown. The only crate that wires implementations together.

mod config;
mod discovery;
mod logging;
mod run;

use std::{process::ExitCode, time::Duration};

use clap::Parser;
use tracing::{error, info};

fn main() -> ExitCode {
    let config = config::Config::parse();
    logging::init(config.log_format);

    // An explicit runtime rather than `#[tokio::main]`, for two reasons: it must
    // be multi-thread, because on a current-thread runtime the BitTorrent engine
    // runs its blocking disk work inline and stalls everything; and it is shut
    // down with a deadline, so a blocking task that never returns cannot keep
    // the process alive after everything else has stopped.
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            error!(%error, "cannot start the async runtime");
            return ExitCode::FAILURE;
        }
    };
    let result = runtime.block_on(run::run(config));
    runtime.shutdown_timeout(Duration::from_secs(2));

    match result {
        Ok(()) => {
            info!("shutdown complete");
            ExitCode::SUCCESS
        }
        Err(error) => {
            error!("{error:#}");
            ExitCode::FAILURE
        }
    }
}
