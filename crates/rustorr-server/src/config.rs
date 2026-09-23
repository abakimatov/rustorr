use std::{net::SocketAddr, path::PathBuf, time::Duration};

use clap::{Parser, ValueEnum};
use ipnet::IpNet;
use rustorr_cache::CacheConfig;

/// Every option can also be given as an environment variable.
#[derive(Debug, Parser)]
#[command(name = "rustorr", version, about = "Torrent streaming server")]
pub struct Config {
    /// Address for the HTTP API.
    #[arg(long, env = "RUSTORR_LISTEN", default_value = "0.0.0.0:8090")]
    pub listen: SocketAddr,

    /// Directory for everything Rustorr stores. It holds the database, the
    /// engine's files and, in disk mode, the cache.
    #[arg(long, env = "RUSTORR_DATA_DIR", default_value = "data")]
    pub data_dir: PathBuf,

    /// Where cached torrent data lives.
    #[arg(long, env = "RUSTORR_CACHE", value_enum, default_value_t = CacheMode::Disk)]
    pub cache: CacheMode,

    /// Cache size to stay under, for example `4GiB` or a number of bytes.
    /// A target, not a hard limit: torrents being watched are never evicted.
    #[arg(
        long,
        env = "RUSTORR_CACHE_SIZE",
        value_parser = parse_size,
        default_value_t = CacheConfig::PROVISIONAL_CAP_BYTES,
    )]
    pub cache_size: u64,

    /// Port for incoming peer connections, TCP and uTP. 0 picks a free port.
    #[arg(long, env = "RUSTORR_PEER_PORT", default_value_t = 0)]
    pub peer_port: u16,

    /// Do not use the DHT to find peers.
    #[arg(long, env = "RUSTORR_DISABLE_DHT")]
    pub disable_dht: bool,

    /// Do not announce to trackers.
    #[arg(long, env = "RUSTORR_DISABLE_TRACKERS")]
    pub disable_trackers: bool,

    /// Require HTTP Basic authentication using `<data-dir>/accs.db`.
    #[arg(long, env = "RUSTORR_HTTP_AUTH")]
    pub http_auth: bool,

    /// Comma-separated proxy CIDRs allowed to supply Forwarded and
    /// X-Forwarded-Host/Proto for generated URLs. WAF decisions always use the
    /// real socket peer, independently of this list.
    #[arg(
        long,
        env = "RUSTORR_TRUSTED_PROXIES",
        value_delimiter = ',',
        default_value = "127.0.0.0/8,::1/128"
    )]
    pub trusted_proxies: Vec<IpNet>,

    /// Seconds to wait for open connections on shutdown before closing them.
    /// Keep it below the container runtime's stop timeout (10 s in Docker).
    #[arg(
        long,
        env = "RUSTORR_SHUTDOWN_GRACE",
        value_parser = parse_seconds,
        default_value = "5",
    )]
    pub shutdown_grace: Duration,

    #[arg(long, env = "RUSTORR_LOG_FORMAT", value_enum, default_value_t = LogFormat::Text)]
    pub log_format: LogFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CacheMode {
    /// On the SSD, with the operating system's page cache keeping recent data
    /// in RAM.
    Disk,
    /// In RAM only.
    Memory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum LogFormat {
    Text,
    Json,
}

fn parse_seconds(text: &str) -> Result<Duration, String> {
    text.trim()
        .parse::<u64>()
        .map(Duration::from_secs)
        .map_err(|_| format!("`{text}` is not a whole number of seconds"))
}

/// A number of bytes, or a number with a binary unit: `KiB`, `MiB`, `GiB`, `TiB`.
/// Decimal units like `GB` are refused rather than guessed: the two differ by
/// 7% at gigabyte scale.
fn parse_size(text: &str) -> Result<u64, String> {
    let text = text.trim();
    let digits_end = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    let (digits, unit) = text.split_at(digits_end);
    let number: u64 = digits
        .parse()
        .map_err(|_| format!("`{text}` does not start with a whole number"))?;

    let shift = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 0,
        "kib" => 10,
        "mib" => 20,
        "gib" => 30,
        "tib" => 40,
        other => {
            return Err(format!(
                "unknown unit `{other}`; use a plain number of bytes or KiB, MiB, GiB, TiB"
            ));
        }
    };
    number
        .checked_mul(1 << shift)
        .ok_or_else(|| format!("`{text}` is too large"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Config {
        Config::try_parse_from(std::iter::once("rustorr").chain(args.iter().copied())).unwrap()
    }

    #[test]
    fn defaults_match_the_documented_ones() {
        let config = parse(&[]);

        assert_eq!(config.listen, "0.0.0.0:8090".parse().unwrap());
        assert_eq!(config.data_dir, PathBuf::from("data"));
        assert_eq!(config.cache, CacheMode::Disk);
        assert_eq!(config.cache_size, CacheConfig::PROVISIONAL_CAP_BYTES);
        assert_eq!(config.peer_port, 0);
        assert!(!config.disable_dht && !config.disable_trackers);
        assert!(!config.http_auth);
        assert_eq!(config.trusted_proxies.len(), 2);
        assert_eq!(config.shutdown_grace, Duration::from_secs(5));
        assert_eq!(config.log_format, LogFormat::Text);
    }

    #[test]
    fn flags_override_the_defaults() {
        let config = parse(&[
            "--listen",
            "127.0.0.1:9000",
            "--data-dir",
            "/var/lib/rustorr",
            "--cache",
            "memory",
            "--cache-size",
            "512MiB",
            "--peer-port",
            "51413",
            "--disable-dht",
            "--disable-trackers",
            "--http-auth",
            "--trusted-proxies",
            "10.0.0.0/8,192.168.0.0/16",
            "--shutdown-grace",
            "2",
            "--log-format",
            "json",
        ]);

        assert_eq!(config.listen.port(), 9000);
        assert_eq!(config.data_dir, PathBuf::from("/var/lib/rustorr"));
        assert_eq!(config.cache, CacheMode::Memory);
        assert_eq!(config.cache_size, 512 << 20);
        assert_eq!(config.peer_port, 51413);
        assert!(config.disable_dht && config.disable_trackers);
        assert!(config.http_auth);
        assert_eq!(config.trusted_proxies.len(), 2);
        assert_eq!(config.shutdown_grace, Duration::from_secs(2));
        assert_eq!(config.log_format, LogFormat::Json);
    }

    #[test]
    fn bad_values_are_rejected() {
        for args in [
            &["--listen", "not-an-address"][..],
            &["--cache", "tape"],
            &["--cache-size", "4GB"],
            &["--peer-port", "70000"],
            &["--shutdown-grace", "soon"],
            &["--no-such-flag"],
        ] {
            assert!(
                Config::try_parse_from(std::iter::once("rustorr").chain(args.iter().copied()))
                    .is_err(),
                "{args:?}"
            );
        }
    }

    #[test]
    fn sizes_take_a_plain_number_or_a_binary_unit() {
        assert_eq!(parse_size("0"), Ok(0));
        assert_eq!(parse_size("1024"), Ok(1024));
        assert_eq!(parse_size("7B"), Ok(7));
        assert_eq!(parse_size("3KiB"), Ok(3 << 10));
        assert_eq!(parse_size("2MiB"), Ok(2 << 20));
        assert_eq!(parse_size("4GiB"), Ok(4 << 30));
        assert_eq!(parse_size("1TiB"), Ok(1 << 40));
    }

    #[test]
    fn size_units_ignore_case_and_surrounding_space() {
        assert_eq!(parse_size(" 4 gib "), Ok(4 << 30));
        assert_eq!(parse_size("8MIB"), Ok(8 << 20));
    }

    #[test]
    fn decimal_units_and_nonsense_are_refused() {
        for bad in [
            "4GB",
            "4G",
            "4 megabytes",
            "",
            "GiB",
            "-1",
            "1.5GiB",
            "0x10",
        ] {
            assert!(parse_size(bad).is_err(), "{bad:?} must not parse");
        }
    }

    #[test]
    fn a_size_that_overflows_is_refused() {
        assert!(parse_size("18446744073709551615").is_ok());
        assert!(parse_size("18446744073709551615KiB").is_err());
        assert!(parse_size("99999999999999999999").is_err());
    }
}
