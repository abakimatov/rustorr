//! `rustorr` binary: configuration, composition root, observability and
//! graceful shutdown. The only crate that wires implementations together.

mod config;
mod discovery;
mod logging;
mod maintenance;
mod run;
mod tls;

use std::{process::ExitCode, time::Duration};

use tracing::{error, info};

fn main() -> ExitCode {
    let mut config = config::Config::from_env_and_args();
    if let Some(command) = config.command.take() {
        return service_command(command, &config);
    }
    if let Err(error) = logging::init(config.log_format, config.log_file.as_deref()) {
        eprintln!("cannot open the log file: {error}");
        return ExitCode::FAILURE;
    }

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

/// `health`, `backup` and `restore`: short-lived, they print to stderr and
/// answer with the exit code.
fn service_command(command: config::Command, config: &config::Config) -> ExitCode {
    let result = match command {
        config::Command::Health { timeout } => maintenance::health(
            maintenance::first_listen(&config.listen),
            Duration::from_secs(timeout),
        )
        .map(|status| eprintln!("healthy: HTTP {status}")),
        config::Command::Backup { file } => maintenance::backup(&config.data_dir, &file)
            .map(|files| eprintln!("backup written to {}: {}", file.display(), files.join(", "))),
        config::Command::Passwd { user } => {
            let mut password = String::new();
            std::io::stdin()
                .read_line(&mut password)
                .map_err(anyhow::Error::from)
                .and_then(|_| {
                    maintenance::set_password(
                        &config.data_dir,
                        &user,
                        password.trim_end_matches(['\r', '\n']),
                    )
                })
                .map(|()| eprintln!("password set for {user}"))
        }
        config::Command::Restore { file, force } => {
            maintenance::restore(&config.data_dir, &file, force).map(|files| {
                eprintln!(
                    "restored into {}: {}",
                    config.data_dir.display(),
                    files.join(", ")
                )
            })
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}
