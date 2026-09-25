use std::{collections::HashMap, net::IpAddr, path::Path, sync::Arc};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use ipnet::IpNet;
use serde::Serialize;

use axum::http::{HeaderMap, Uri, header};
use rustorr_lifecycle::WafLists;
use tokio::sync::Notify;

const DEFAULT_BLOCKED_REFERERS: &[&str] = &[
    "abhq.ru",
    "abmsx.tech",
    "akter.black",
    "bylampa.online",
    "lampa.click",
    "lampa.land",
    "lampa1.ru",
    "line.pm",
    "nnmtv.pw",
    "tvigl.info",
    "uspeh.sbs",
    "usph.xyz",
    "xabb.ru",
];

#[derive(Debug, Clone, Default)]
pub struct Credentials(Arc<HashMap<String, String>>);

impl Credentials {
    pub fn read(path: &Path) -> Result<Self, String> {
        let bytes = std::fs::read(path)
            .map_err(|_| format!("cannot read authentication database {}", path.display()))?;
        let accounts: HashMap<String, String> = serde_json::from_slice(&bytes)
            .map_err(|_| format!("authentication database {} is invalid", path.display()))?;
        if accounts.is_empty()
            || accounts
                .iter()
                .any(|(user, password)| user.is_empty() || password.is_empty())
        {
            return Err(format!(
                "authentication database {} contains no usable accounts",
                path.display()
            ));
        }
        Ok(Self(Arc::new(accounts)))
    }

    pub fn recognizes(&self, headers: &HeaderMap) -> bool {
        let Some(value) = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Basic "))
            .and_then(|value| STANDARD.decode(value).ok())
            .and_then(|value| String::from_utf8(value).ok())
        else {
            return false;
        };
        let Some((user, password)) = value.split_once(':') else {
            return false;
        };
        self.0
            .get(user)
            .is_some_and(|expected| expected == password)
    }
}

#[derive(Clone)]
pub struct HttpConfig {
    pub credentials: Option<Credentials>,
    pub trusted_proxies: Vec<IpNet>,
    /// Signalled by `GET /shutdown`; the server stops as it does on SIGTERM.
    pub shutdown: Option<Arc<Notify>>,
    /// Read-only DB mode: management writes are refused with 403.
    pub read_only: bool,
    /// Streams of larger files are refused with 403.
    pub max_stream_size: Option<u64>,
    /// Search routes skip HTTP authentication.
    pub search_without_auth: bool,
    /// `/dav` serves the torrent file system.
    pub webdav: bool,
    /// The web server's port, which `/ffp` probes its own `/play` URL on.
    pub port: u16,
    /// The `ffprobe` binary `/ffp` runs.
    pub ffprobe: std::path::PathBuf,
    /// The web log (`--weblogpath`): one line per request.
    pub access_log: Option<std::sync::Arc<crate::AccessLog>>,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            credentials: None,
            shutdown: None,
            read_only: false,
            max_stream_size: None,
            search_without_auth: false,
            webdav: false,
            port: 8090,
            ffprobe: std::path::PathBuf::from("ffprobe"),
            access_log: None,
            trusted_proxies: vec![
                "127.0.0.0/8".parse().expect("loopback CIDR"),
                "::1/128".parse().expect("loopback CIDR"),
            ],
        }
    }
}

impl HttpConfig {
    pub fn authorized(&self, headers: &HeaderMap) -> bool {
        self.credentials
            .as_ref()
            .is_none_or(|credentials| credentials.recognizes(headers))
    }

    pub fn auth_enabled(&self) -> bool {
        self.credentials.is_some()
    }

    fn trusts(&self, peer: IpAddr) -> bool {
        self.trusted_proxies
            .iter()
            .any(|network| network.contains(&peer))
    }

    /// The scheme clients reached the server with, as forwarded by a trusted
    /// proxy.
    pub fn public_scheme(&self, peer: IpAddr, headers: &HeaderMap, uri: &Uri) -> String {
        let trusted = self.trusts(peer);
        let forwarded = trusted
            .then(|| headers.get("forwarded")?.to_str().ok())
            .flatten()
            .and_then(parse_forwarded);
        forwarded
            .as_ref()
            .and_then(|(proto, _)| proto.as_deref())
            .or_else(|| {
                trusted
                    .then(|| first_header(headers, "x-forwarded-proto"))
                    .flatten()
            })
            .filter(|scheme| matches!(*scheme, "http" | "https"))
            .unwrap_or_else(|| uri.scheme_str().unwrap_or("http"))
            .to_owned()
    }

    pub fn public_base(&self, peer: IpAddr, headers: &HeaderMap, uri: &Uri) -> String {
        let trusted = self.trusts(peer);
        let forwarded = trusted
            .then(|| headers.get("forwarded")?.to_str().ok())
            .flatten()
            .and_then(parse_forwarded);
        let scheme = self.public_scheme(peer, headers, uri);
        let host = forwarded
            .as_ref()
            .and_then(|(_, host)| host.as_deref())
            .or_else(|| {
                trusted
                    .then(|| first_header(headers, "x-forwarded-host"))
                    .flatten()
            })
            .or_else(|| {
                headers
                    .get(header::HOST)
                    .and_then(|value| value.to_str().ok())
            })
            .unwrap_or("127.0.0.1:8090");
        format!("{scheme}://{host}")
    }
}

fn first_header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(name)?
        .to_str()
        .ok()?
        .split(',')
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn parse_forwarded(value: &str) -> Option<(Option<String>, Option<String>)> {
    let mut proto = None;
    let mut host = None;
    for field in value.split(',').next()?.split(';') {
        let (name, value) = field.trim().split_once('=')?;
        let value = value.trim_matches('"');
        match name.to_ascii_lowercase().as_str() {
            "proto" => proto = Some(value.to_ascii_lowercase()),
            "host" => host = Some(value.to_owned()),
            _ => {}
        }
    }
    Some((proto, host))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WafWarning {
    pub list: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    pub code: &'static str,
}

#[derive(Debug, Clone)]
enum IpRule {
    Address(IpAddr),
    Network(IpNet),
    Range(IpAddr, IpAddr),
}

impl IpRule {
    fn contains(&self, address: IpAddr) -> bool {
        match self {
            Self::Address(expected) => *expected == address,
            Self::Network(network) => network.contains(&address),
            Self::Range(first, last) => ip_number(*first)
                .zip(ip_number(*last))
                .zip(ip_number(address))
                .is_some_and(|((first, last), address)| first <= address && address <= last),
        }
    }
}

fn ip_number(address: IpAddr) -> Option<(u8, u128)> {
    match address {
        IpAddr::V4(address) => Some((4, u32::from(address).into())),
        IpAddr::V6(address) => Some((6, u128::from(address))),
    }
}

#[derive(Debug, Clone)]
pub struct WafSnapshot {
    pub warnings: Vec<WafWarning>,
    whitelist: Vec<IpRule>,
    blacklist: Vec<IpRule>,
    referers: Vec<String>,
}

impl WafSnapshot {
    pub fn parse(lists: WafLists) -> Self {
        let (whitelist, mut warnings) = parse_ip_rules("whitelist", &lists.whitelist);
        let (blacklist, black_warnings) = parse_ip_rules("blacklist", &lists.blacklist);
        warnings.extend(black_warnings);
        let (user_referers, referer_warnings) = parse_referers(&lists.referers);
        warnings.extend(referer_warnings);
        let mut referers: Vec<_> = DEFAULT_BLOCKED_REFERERS
            .iter()
            .map(|host| (*host).to_owned())
            .collect();
        for host in user_referers {
            if !referers.contains(&host) {
                referers.push(host);
            }
        }
        Self {
            warnings,
            whitelist,
            blacklist,
            referers,
        }
    }

    pub fn ip_enabled(&self) -> bool {
        !self.whitelist.is_empty() || !self.blacklist.is_empty()
    }

    pub fn referer_enabled(&self) -> bool {
        !self.referers.is_empty()
    }

    pub fn blocks(&self, peer: IpAddr, headers: &HeaderMap) -> bool {
        if [header::REFERER.as_str(), header::ORIGIN.as_str()]
            .into_iter()
            .filter_map(|name| headers.get(name)?.to_str().ok())
            .filter_map(header_host)
            .any(|host| {
                self.referers
                    .iter()
                    .any(|blocked| host == *blocked || host.ends_with(&format!(".{blocked}")))
            })
        {
            return true;
        }
        if !self.whitelist.is_empty() && !self.whitelist.iter().any(|rule| rule.contains(peer)) {
            return true;
        }
        self.blacklist.iter().any(|rule| rule.contains(peer))
    }
}

fn parse_ip_rules(list: &'static str, text: &str) -> (Vec<IpRule>, Vec<WafWarning>) {
    let mut rules = Vec::new();
    let mut warnings = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        let raw = raw.trim();
        if raw.is_empty() || raw.starts_with('#') {
            continue;
        }
        let candidates = [
            raw,
            raw.split_once(':').map_or(raw, |(_, value)| value.trim()),
        ];
        let rule = candidates.into_iter().find_map(|value| {
            value
                .parse::<IpNet>()
                .ok()
                .map(IpRule::Network)
                .or_else(|| value.parse::<IpAddr>().ok().map(IpRule::Address))
                .or_else(|| {
                    let (first, last) = value.split_once('-')?;
                    Some(IpRule::Range(
                        first.trim().parse().ok()?,
                        last.trim().parse().ok()?,
                    ))
                })
        });
        if let Some(rule) = rule {
            rules.push(rule);
        } else {
            warnings.push(WafWarning {
                list,
                line: Some(index + 1),
                code: "invalid_ip_range",
            });
        }
    }
    (rules, warnings)
}

fn parse_referers(text: &str) -> (Vec<String>, Vec<WafWarning>) {
    let mut hosts = Vec::new();
    let mut warnings = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        let raw = raw.trim();
        if raw.is_empty() || raw.starts_with('#') {
            continue;
        }
        let host = if raw.contains('*')
            || (raw.chars().any(|character| "/?#@".contains(character)) && !raw.contains("://"))
        {
            None
        } else if let Some((_, rest)) = raw.split_once("://") {
            rest.trim_end_matches('/')
                .split(':')
                .next()
                .map(str::to_owned)
        } else {
            raw.split(':').next().map(str::to_owned)
        };
        if let Some(host) = host.filter(|host| !host.is_empty()) {
            hosts.push(host.trim_matches(['[', ']', '.']).to_ascii_lowercase());
        } else {
            warnings.push(WafWarning {
                list: "referers",
                line: Some(index + 1),
                code: "invalid_referer",
            });
        }
    }
    (hosts, warnings)
}

fn header_host(value: &str) -> Option<String> {
    let value = value.split_once("://")?.1;
    let authority = value.split('/').next()?;
    let host = authority
        .trim_start_matches('[')
        .split([']', ':'])
        .next()?
        .trim_end_matches('.')
        .to_ascii_lowercase();
    (!host.is_empty()).then_some(host)
}

#[cfg(test)]
mod tests {
    use std::{fs, net::Ipv4Addr};

    use axum::http::{HeaderMap, HeaderName, HeaderValue};

    use super::*;

    #[test]
    fn authentication_database_must_exist_and_contain_usable_accounts() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("accs.db");
        assert!(Credentials::read(&path).is_err());

        for invalid in [b"".as_slice(), b"not-json", b"{}", br#"{"user":""}"#] {
            fs::write(&path, invalid).unwrap();
            assert!(Credentials::read(&path).is_err());
        }

        fs::write(&path, br#"{"user":"secret"}"#).unwrap();
        let credentials = Credentials::read(&path).unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Basic dXNlcjpzZWNyZXQ="),
        );
        assert!(credentials.recognizes(&headers));
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Basic dXNlcjp3cm9uZw=="),
        );
        assert!(!credentials.recognizes(&headers));
    }

    #[test]
    fn forwarded_location_is_accepted_only_from_a_trusted_peer() {
        let config = HttpConfig::default();
        let mut headers = HeaderMap::new();
        headers.insert(
            header::HOST,
            HeaderValue::from_static("direct.invalid:8090"),
        );
        headers.insert(
            HeaderName::from_static("x-forwarded-host"),
            HeaderValue::from_static("proxy.invalid"),
        );
        headers.insert(
            HeaderName::from_static("x-forwarded-proto"),
            HeaderValue::from_static("https"),
        );
        let uri: Uri = "/playlist".parse().unwrap();

        assert_eq!(
            config.public_base(Ipv4Addr::LOCALHOST.into(), &headers, &uri),
            "https://proxy.invalid"
        );
        assert_eq!(
            config.public_base(Ipv4Addr::new(10, 0, 0, 2).into(), &headers, &uri),
            "http://direct.invalid:8090"
        );
    }

    #[test]
    fn waf_uses_the_socket_peer_and_reports_invalid_rules() {
        let snapshot = WafSnapshot::parse(WafLists {
            whitelist: "10.0.0.0/8\ninvalid".into(),
            blacklist: "10.1.2.3".into(),
            referers: "blocked.invalid".into(),
        });
        assert_eq!(snapshot.warnings.len(), 1);
        assert!(snapshot.blocks(Ipv4Addr::new(192, 0, 2, 1).into(), &HeaderMap::new()));
        assert!(snapshot.blocks(Ipv4Addr::new(10, 1, 2, 3).into(), &HeaderMap::new()));
        assert!(!snapshot.blocks(Ipv4Addr::new(10, 1, 2, 4).into(), &HeaderMap::new()));

        let mut headers = HeaderMap::new();
        headers.insert(
            header::REFERER,
            HeaderValue::from_static("https://blocked.invalid/watch"),
        );
        assert!(snapshot.blocks(Ipv4Addr::new(10, 1, 2, 4).into(), &headers));
    }
}
