use std::{
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    time::Duration,
};

use clap::{Parser, ValueEnum};
use ipnet::IpNet;
use rustorr_cache::CacheConfig;

/// Every option can also be given as an environment variable.
#[derive(Debug, Parser)]
#[command(name = "rustorr", version, about = "Torrent streaming server")]
pub struct Config {
    /// Address for the HTTP API. Repeat the flag, or separate addresses with
    /// commas, to listen on several, as MatriX.145's repeatable `--ip`
    /// does; generated URLs and DLNA use the first address's port.
    #[arg(
        long,
        env = "RUSTORR_LISTEN",
        value_delimiter = ',',
        default_value = "0.0.0.0:8090"
    )]
    pub listen: Vec<SocketAddr>,

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

    /// Never write to the database: settings, catalog, viewed and WAF changes
    /// are refused or ignored, as with MatriX.145's read-only DB mode.
    #[arg(long, env = "RUSTORR_READ_ONLY")]
    pub read_only: bool,

    /// Refuse to stream files larger than this, for example `20GiB`.
    #[arg(long, env = "RUSTORR_MAX_STREAM_SIZE", value_parser = parse_size)]
    pub max_stream_size: Option<u64>,

    /// Add every `.torrent` file that appears in this directory to the
    /// catalog, then delete the file.
    #[arg(long, env = "RUSTORR_TORRENTS_DIR")]
    pub torrents_dir: Option<PathBuf>,

    /// Serve the search routes without HTTP authentication.
    #[arg(long, env = "RUSTORR_SEARCH_WITHOUT_AUTH")]
    pub search_without_auth: bool,

    /// Serve the torrent file system over WebDAV at `/dav`, without HTTP
    /// authentication, as MatriX.145's `--webdav` does.
    #[arg(long, env = "RUSTORR_WEBDAV")]
    pub webdav: bool,

    /// Mount the torrent file system here with FUSE, read-only, as
    /// MatriX.145's `--fusepath` does. Needs `/dev/fuse` and the right to
    /// mount (root with `CAP_SYS_ADMIN`, or `fusermount3`).
    #[arg(long, env = "RUSTORR_FUSE_PATH")]
    pub fuse_path: Option<PathBuf>,

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

    /// Write the server log to this file instead of stderr, as MatriX.145's
    /// `--logpath`. A file of 100 MiB or more is started afresh.
    #[arg(long, env = "RUSTORR_LOG_FILE")]
    pub log_file: Option<PathBuf>,

    /// Append one line per HTTP request to this file, in the format of
    /// MatriX.145's `--weblogpath` (status, client IP, method, path, request
    /// body). It may be the same file as `--log-file`.
    #[arg(long, env = "RUSTORR_ACCESS_LOG_FILE")]
    pub access_log_file: Option<PathBuf>,

    /// Keep running on SIGINT, SIGTERM, SIGHUP and SIGQUIT, as MatriX.145's
    /// `--dontkill`; the server then stops only through `GET /shutdown`.
    #[arg(long, env = "RUSTORR_DONT_KILL")]
    pub dont_kill: bool,

    /// Peer listener address, `HOST:PORT` or `:PORT`, as MatriX.145's
    /// `--torrentaddr`. Overrides `--peer-port`.
    #[arg(long, env = "RUSTORR_TORRENT_ADDR", value_parser = parse_torrent_addr)]
    pub torrent_addr: Option<TorrentAddr>,

    /// Public IPv4 address, as MatriX.145's `--pubipv4`. Checked and logged;
    /// the engine (librqbit 9.0.1) cannot announce it.
    #[arg(long, env = "RUSTORR_PUBLIC_IPV4")]
    pub public_ipv4: Option<String>,

    /// Public IPv6 address, as MatriX.145's `--pubipv6`. Checked and logged;
    /// the engine cannot announce it.
    #[arg(long, env = "RUSTORR_PUBLIC_IPV6")]
    pub public_ipv6: Option<String>,

    /// Proxy for BitTorrent traffic, as MatriX.145's `--proxyurl`. Only
    /// `socks5://[user:password@]host:port`: the engine supports no other.
    #[arg(long, env = "RUSTORR_PROXY_URL")]
    pub proxy_url: Option<String>,

    /// What goes through `--proxy-url`: `peers` or `full`. Both send peer
    /// connections and HTTP tracker requests through it; MatriX.145's
    /// default `tracker` (trackers only) is not possible with this engine.
    #[arg(long, env = "RUSTORR_PROXY_MODE")]
    pub proxy_mode: Option<String>,
}

/// `--torrentaddr`: an optional host and a port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TorrentAddr {
    pub ip: Option<IpAddr>,
    pub port: u16,
}

fn parse_torrent_addr(text: &str) -> Result<TorrentAddr, String> {
    let invalid = || format!("`{text}` is not HOST:PORT or :PORT");
    let (host, port) = text.trim().rsplit_once(':').ok_or_else(invalid)?;
    let port = port.parse::<u16>().map_err(|_| invalid())?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let ip = if host.is_empty() {
        None
    } else {
        Some(host.parse::<IpAddr>().map_err(|_| invalid())?)
    };
    Ok(TorrentAddr { ip, port })
}

/// Why a `--public-ipv4`/`--public-ipv6` value is not used, in MatriX.145's
/// terms: it must parse as the right family and not be private.
pub fn public_ip(value: &str, ipv6: bool) -> Result<IpAddr, &'static str> {
    let ip: IpAddr = value.trim().parse().map_err(|_| "not an IP address")?;
    let ip = ip.to_canonical();
    match (ip, ipv6) {
        (IpAddr::V4(_), true) => return Err("not an IPv6 address"),
        (IpAddr::V6(_), false) => return Err("not an IPv4 address"),
        _ => {}
    }
    let private = match ip {
        IpAddr::V4(ip) => {
            ip.is_loopback()
                || ip.is_private()
                || ip.is_link_local()
                || (ip.octets()[0] == 224 && ip.octets()[1] == 0 && ip.octets()[2] == 0)
        }
        IpAddr::V6(ip) => {
            let first = ip.segments()[0];
            ip.is_loopback()
                || (first & 0xffc0) == 0xfe80
                || (first & 0xfe00) == 0xfc00
                || (first & 0xff0f) == 0xff02
        }
    };
    if private {
        Err("a private address")
    } else {
        Ok(ip)
    }
}

/// The proxy the engine gets from `--proxy-url` and `--proxy-mode`. As in
/// MatriX.145 the mode defaults to `tracker` and an unknown one falls back to
/// it (with a warning), which this engine cannot honour.
pub fn engine_proxy(url: Option<&str>, mode: Option<&str>) -> Result<Option<String>, String> {
    let Some(url) = url.filter(|url| !url.is_empty()) else {
        return Ok(None);
    };
    let mut mode = mode.filter(|mode| !mode.is_empty()).unwrap_or("tracker");
    if !matches!(mode, "tracker" | "peers" | "full") {
        tracing::warn!(mode, "invalid proxy mode, using the default `tracker`");
        mode = "tracker";
    }
    let scheme = url.split_once("://").map_or("", |(scheme, _)| scheme);
    if scheme != "socks5" {
        return Err(format!(
            "--proxy-url: the BitTorrent engine supports only socks5:// proxies, not `{scheme}`"
        ));
    }
    if mode == "tracker" {
        return Err(
            "--proxy-mode tracker (the default) is not supported: the engine cannot \
             proxy HTTP trackers without peer connections; use --proxy-mode peers or full"
                .into(),
        );
    }
    Ok(Some(url.to_string()))
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

        assert_eq!(config.listen, vec!["0.0.0.0:8090".parse().unwrap()]);
        assert_eq!(config.log_file, None);
        assert_eq!(config.access_log_file, None);
        assert!(!config.dont_kill);
        assert_eq!(config.torrent_addr, None);
        assert_eq!(config.proxy_url, None);
        assert_eq!(config.data_dir, PathBuf::from("data"));
        assert_eq!(config.cache, CacheMode::Disk);
        assert_eq!(config.cache_size, CacheConfig::PROVISIONAL_CAP_BYTES);
        assert_eq!(config.peer_port, 0);
        assert!(!config.disable_dht && !config.disable_trackers);
        assert!(!config.http_auth);
        assert_eq!(config.trusted_proxies.len(), 2);
        assert!(!config.read_only && !config.search_without_auth && !config.webdav);
        assert_eq!(config.max_stream_size, None);
        assert_eq!(config.torrents_dir, None);
        assert_eq!(config.fuse_path, None);
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
            "--read-only",
            "--max-stream-size",
            "20GiB",
            "--torrents-dir",
            "/var/lib/rustorr/incoming",
            "--search-without-auth",
        ]);

        assert_eq!(config.listen, vec!["127.0.0.1:9000".parse().unwrap()]);
        assert_eq!(config.data_dir, PathBuf::from("/var/lib/rustorr"));
        assert_eq!(config.cache, CacheMode::Memory);
        assert_eq!(config.cache_size, 512 << 20);
        assert_eq!(config.peer_port, 51413);
        assert!(config.disable_dht && config.disable_trackers);
        assert!(config.http_auth);
        assert_eq!(config.trusted_proxies.len(), 2);
        assert_eq!(config.shutdown_grace, Duration::from_secs(2));
        assert_eq!(config.log_format, LogFormat::Json);
        assert!(config.read_only && config.search_without_auth);
        assert_eq!(config.max_stream_size, Some(20 << 30));
        assert_eq!(
            config.torrents_dir,
            Some(PathBuf::from("/var/lib/rustorr/incoming"))
        );
    }

    #[test]
    fn process_flags_parse() {
        let config = parse(&[
            "--listen",
            "127.0.0.1:9000,10.0.0.1:9000",
            "--listen",
            "[::1]:9000",
            "--log-file",
            "/var/log/rustorr.log",
            "--access-log-file",
            "/var/log/web.log",
            "--dont-kill",
            "--torrent-addr",
            ":1337",
        ]);
        assert_eq!(config.listen.len(), 3);
        assert_eq!(config.listen[2], "[::1]:9000".parse().unwrap());
        assert_eq!(config.log_file, Some(PathBuf::from("/var/log/rustorr.log")));
        assert_eq!(
            config.access_log_file,
            Some(PathBuf::from("/var/log/web.log"))
        );
        assert!(config.dont_kill);
        assert_eq!(
            config.torrent_addr,
            Some(TorrentAddr {
                ip: None,
                port: 1337
            })
        );
        assert_eq!(
            parse_torrent_addr("127.0.0.1:1337"),
            Ok(TorrentAddr {
                ip: Some("127.0.0.1".parse().unwrap()),
                port: 1337
            })
        );
        assert_eq!(
            parse_torrent_addr("[::1]:1337").unwrap().ip,
            Some("::1".parse().unwrap())
        );
        for bad in ["1337", "host:1337", ":99999", ""] {
            assert!(parse_torrent_addr(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn public_addresses_are_checked_like_the_reference() {
        assert_eq!(public_ip("8.8.8.8", false), Ok("8.8.8.8".parse().unwrap()));
        assert_eq!(
            public_ip("::ffff:8.8.8.8", false),
            Ok("8.8.8.8".parse().unwrap())
        );
        assert_eq!(
            public_ip("2001:db8::1", true),
            Ok("2001:db8::1".parse().unwrap())
        );
        for (value, ipv6) in [
            ("10.1.2.3", false),
            ("172.20.0.1", false),
            ("192.168.1.1", false),
            ("127.0.0.1", false),
            ("169.254.1.1", false),
            ("::1", true),
            ("fe80::1", true),
            ("fd00::1", true),
            ("2001:db8::1", false),
            ("8.8.8.8", true),
            ("nonsense", false),
        ] {
            assert!(public_ip(value, ipv6).is_err(), "{value}");
        }
    }

    #[test]
    fn only_a_socks5_proxy_for_peers_can_be_applied() {
        assert_eq!(engine_proxy(None, Some("full")), Ok(None));
        assert_eq!(
            engine_proxy(Some("socks5://u:p@10.0.0.1:1080"), Some("full")),
            Ok(Some("socks5://u:p@10.0.0.1:1080".into()))
        );
        assert!(engine_proxy(Some("socks5://10.0.0.1:1080"), Some("peers")).is_ok());
        let tracker = engine_proxy(Some("socks5://10.0.0.1:1080"), None).unwrap_err();
        assert!(tracker.contains("--proxy-mode tracker"), "{tracker}");
        assert!(engine_proxy(Some("socks5://10.0.0.1:1080"), Some("everything")).is_err());
        for scheme in ["http", "https", "socks4", "socks4a", "socks5h"] {
            let error =
                engine_proxy(Some(&format!("{scheme}://10.0.0.1:1080")), Some("full")).unwrap_err();
            assert!(error.contains(scheme), "{error}");
        }
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
            &["--torrent-addr", "1337"],
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
