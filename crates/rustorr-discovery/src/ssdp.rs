//! SSDP for the DLNA media server, as `anacrolix/dms/ssdp` runs it: one
//! server per multicast interface, `ssdp:alive` for every type on every
//! interface address each notify interval, answers to `M-SEARCH` after a
//! random delay within `MX`, and `ssdp:byebye` on stop.

use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4},
    sync::Arc,
    time::Duration,
};

use rand::Rng;
use tokio::{sync::Mutex, task::JoinHandle};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::{
    interfaces::{self, Interface},
    socket::MulticastSocket,
};

const GROUP: Ipv4Addr = Ipv4Addr::new(239, 255, 255, 250);
const PORT: u16 = 1900;
const HOST: &str = "239.255.255.250:1900";
const MX_MAX: u64 = 10;
/// The `SERVER` value dms sends.
pub const SERVER: &str = "Linux/3.4 DLNADOC/1.50 UPnP/1.0 dms/1";
/// The device and services a TorrServer media server announces.
pub const DEVICE_TYPE: &str = "urn:schemas-upnp-org:device:MediaServer:1";
pub const SERVICE_TYPES: [&str; 3] = [
    "urn:schemas-upnp-org:service:ContentDirectory:1",
    "urn:schemas-upnp-org:service:ConnectionManager:1",
    "urn:microsoft.com:service:X_MS_MediaReceiverRegistrar:1",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SsdpConfig {
    /// `uuid:…` of the root device.
    pub uuid: String,
    /// Port of the DLNA HTTP server that serves `/rootDesc.xml`.
    pub http_port: u16,
    pub notify_interval: Duration,
}

impl SsdpConfig {
    fn types(&self) -> Vec<String> {
        let mut types = vec![
            "upnp:rootdevice".to_owned(),
            self.uuid.clone(),
            DEVICE_TYPE.into(),
        ];
        types.extend(SERVICE_TYPES.map(String::from));
        types
    }

    fn usn(&self, target: &str) -> String {
        if target == self.uuid {
            target.to_owned()
        } else {
            format!("{}::{target}", self.uuid)
        }
    }

    fn location(&self, ip: IpAddr) -> String {
        format!(
            "http://{}/rootDesc.xml",
            SocketAddr::new(ip, self.http_port)
        )
    }

    fn max_age(&self) -> u64 {
        5 * self.notify_interval.as_secs() / 2
    }

    fn notify(&self, target: &str, nts: &str, location: Option<&str>) -> Vec<u8> {
        let mut message = format!(
            "NOTIFY * HTTP/1.1\r\nHOST: {HOST}\r\nNT: {target}\r\nNTS: {nts}\r\nSERVER: {SERVER}\r\nUSN: {}\r\n",
            self.usn(target)
        );
        if let Some(location) = location {
            message.push_str(&format!(
                "CACHE-CONTROL: max-age={}\r\nLOCATION: {location}\r\n",
                self.max_age()
            ));
        }
        message.push_str("\r\n");
        message.into_bytes()
    }

    /// A search answer, headers as Go's `http.Response.Write` orders and
    /// cases them.
    fn response(&self, target: &str, ip: IpAddr) -> Vec<u8> {
        format!(
            "HTTP/1.1 200 OK\r\nCache-Control: max-age={}\r\nExt: \r\nLocation: {}\r\nServer: {SERVER}\r\nSt: {target}\r\nUsn: {}\r\nContent-Length: 0\r\n\r\n",
            self.max_age(),
            self.location(ip),
            self.usn(target)
        )
        .into_bytes()
    }
}

/// The SSDP side of the DLNA server.
#[derive(Default)]
pub struct Ssdp {
    running: Mutex<Vec<Server>>,
}

impl Ssdp {
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts a server on each up, multicast, non-loopback interface.
    pub async fn start(&self, config: SsdpConfig) {
        let mut running = self.running.lock().await;
        stop_all(&mut running).await;
        let config = Arc::new(config);
        for interface in interfaces::list()
            .into_iter()
            .filter(|interface| !interface.loopback && interface.up && interface.multicast)
        {
            match Server::start(Arc::clone(&config), interface.clone()) {
                Ok(server) => {
                    info!(interface = %interface.name, "started SSDP");
                    running.push(server);
                }
                Err(error) => warn!(interface = %interface.name, %error, "cannot start SSDP"),
            }
        }
    }

    pub async fn stop(&self) {
        stop_all(&mut *self.running.lock().await).await;
    }
}

async fn stop_all(running: &mut Vec<Server>) {
    for server in running.drain(..) {
        server.stop().await;
    }
}

struct Server {
    stop: CancellationToken,
    tasks: Vec<JoinHandle<()>>,
    socket: Arc<MulticastSocket>,
    config: Arc<SsdpConfig>,
    interface: Arc<Interface>,
}

impl Server {
    fn start(config: Arc<SsdpConfig>, interface: Interface) -> std::io::Result<Self> {
        let socket = Arc::new(MulticastSocket::open(
            GROUP,
            GROUP,
            PORT,
            std::slice::from_ref(&interface),
            Some(2),
        )?);
        let interface = Arc::new(interface);
        let stop = CancellationToken::new();
        let tasks = vec![
            tokio::spawn(receive(
                Arc::clone(&socket),
                Arc::clone(&config),
                Arc::clone(&interface),
                stop.clone(),
            )),
            tokio::spawn(advertise(
                Arc::clone(&socket),
                Arc::clone(&config),
                Arc::clone(&interface),
                stop.clone(),
            )),
        ];
        Ok(Self {
            stop,
            tasks,
            socket,
            config,
            interface,
        })
    }

    async fn stop(self) {
        self.stop.cancel();
        for task in self.tasks {
            let _ = task.await;
        }
        let group = SocketAddr::V4(SocketAddrV4::new(GROUP, PORT));
        for target in self.config.types() {
            let byebye = self.config.notify(&target, "ssdp:byebye", None);
            let _ = self
                .socket
                .send(&byebye, group, Some(&self.interface))
                .await;
        }
    }
}

/// Sends `bytes` after `delay`, unless the server stops first.
fn delayed(
    socket: &Arc<MulticastSocket>,
    interface: &Arc<Interface>,
    stop: &CancellationToken,
    delay: Duration,
    bytes: Vec<u8>,
    to: SocketAddr,
) {
    let (socket, interface, stop) = (Arc::clone(socket), Arc::clone(interface), stop.clone());
    tokio::spawn(async move {
        tokio::select! {
            () = stop.cancelled() => {}
            () = tokio::time::sleep(delay) => {
                let _ = socket.send(&bytes, to, Some(&interface)).await;
            }
        }
    });
}

async fn advertise(
    socket: Arc<MulticastSocket>,
    config: Arc<SsdpConfig>,
    interface: Arc<Interface>,
    stop: CancellationToken,
) {
    let group = SocketAddr::V4(SocketAddrV4::new(GROUP, PORT));
    loop {
        for address in &interface.addresses {
            if interfaces::link_local(address.ip) {
                continue;
            }
            let location = config.location(address.ip);
            for target in config.types() {
                let delay = Duration::from_millis(rand::rng().random_range(0..100));
                let alive = config.notify(&target, "ssdp:alive", Some(&location));
                delayed(&socket, &interface, &stop, delay, alive, group);
            }
        }
        tokio::select! {
            () = stop.cancelled() => return,
            () = tokio::time::sleep(config.notify_interval) => {}
        }
    }
}

async fn receive(
    socket: Arc<MulticastSocket>,
    config: Arc<SsdpConfig>,
    interface: Arc<Interface>,
    stop: CancellationToken,
) {
    let mut buffer = vec![0; 65536];
    loop {
        let received = tokio::select! {
            () = stop.cancelled() => return,
            received = socket.recv_from(&mut buffer) => received,
        };
        let Ok((length, sender)) = received else {
            continue;
        };
        let Some(search) = Search::parse(&buffer[..length]) else {
            continue;
        };
        for (delay, response) in search.answers(&config, &interface, sender.ip()) {
            delayed(&socket, &interface, &stop, delay, response, sender);
        }
    }
}

/// An `M-SEARCH` request.
#[derive(Debug, PartialEq, Eq)]
struct Search {
    /// `MX` in seconds, clamped to 1..=10.
    mx: u64,
    target: String,
}

impl Search {
    /// `ssdp.ReadRequest` plus the checks of `Server.handle`; `None` for
    /// anything the reference ignores.
    fn parse(bytes: &[u8]) -> Option<Self> {
        let text = std::str::from_utf8(bytes).ok()?;
        let mut lines = text.split("\r\n");
        let mut request = lines.next()?.splitn(3, ' ');
        let method = request.next()?;
        if request.next()? != "*" || !request.next()?.trim().starts_with("HTTP/") {
            return None;
        }
        let mut headers = HashMap::new();
        for line in lines {
            if line.is_empty() {
                break;
            }
            let (name, value) = line.split_once(':')?;
            headers
                .entry(name.trim().to_ascii_lowercase())
                .or_insert_with(|| value.trim().to_owned());
        }
        if method != "M-SEARCH"
            || headers.get("man").map(String::as_str) != Some("\"ssdp:discover\"")
        {
            return None;
        }
        let mut mx = 0;
        if headers.get("host").map(String::as_str) == Some(HOST) {
            mx = parse_go_uint(headers.get("mx").map_or("", String::as_str))?;
        }
        Some(Self {
            mx: mx.clamp(1, MX_MAX),
            target: headers.get("st").cloned().unwrap_or_default(),
        })
    }

    /// Each answer with its random delay: every matching type for every
    /// interface address in the sender's network.
    fn answers(
        &self,
        config: &SsdpConfig,
        interface: &Interface,
        sender: IpAddr,
    ) -> Vec<(Duration, Vec<u8>)> {
        let types: Vec<String> = if self.target == "ssdp:all" {
            config.types()
        } else {
            config
                .types()
                .into_iter()
                .filter(|target| *target == self.target)
                .collect()
        };
        let mut answers = Vec::new();
        for address in interface
            .addresses
            .iter()
            .filter(|address| address.contains(sender))
        {
            for target in &types {
                let delay = Duration::from_millis(rand::rng().random_range(0..self.mx * 1000));
                answers.push((delay, config.response(target, address.ip)));
            }
        }
        answers
    }
}

/// `strconv.ParseUint(s, 0, 0)`: decimal, `0x` hex, `0o`/`0` octal, `0b`.
fn parse_go_uint(text: &str) -> Option<u64> {
    let digits = text.replace('_', "");
    let (radix, digits) = if let Some(rest) = digits
        .strip_prefix("0x")
        .or_else(|| digits.strip_prefix("0X"))
    {
        (16, rest.to_owned())
    } else if let Some(rest) = digits
        .strip_prefix("0b")
        .or_else(|| digits.strip_prefix("0B"))
    {
        (2, rest.to_owned())
    } else if let Some(rest) = digits
        .strip_prefix("0o")
        .or_else(|| digits.strip_prefix("0O"))
    {
        (8, rest.to_owned())
    } else if digits.len() > 1 && digits.starts_with('0') {
        (8, digits[1..].to_owned())
    } else {
        (10, digits)
    };
    u64::from_str_radix(&digits, radix).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interfaces::Address;

    fn config() -> SsdpConfig {
        SsdpConfig {
            uuid: "uuid:9c75442a-03bc-b28c-d59b-05f614916334".into(),
            http_port: 9080,
            notify_interval: Duration::from_secs(30),
        }
    }

    #[test]
    fn notifications_match_the_reference_bytes() {
        // Observed from MatriX.145.
        assert_eq!(
            String::from_utf8(config().notify(
                "upnp:rootdevice",
                "ssdp:alive",
                Some("http://10.0.0.2:9080/rootDesc.xml")
            ))
            .unwrap(),
            "NOTIFY * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nNT: upnp:rootdevice\r\nNTS: ssdp:alive\r\n\
             SERVER: Linux/3.4 DLNADOC/1.50 UPnP/1.0 dms/1\r\n\
             USN: uuid:9c75442a-03bc-b28c-d59b-05f614916334::upnp:rootdevice\r\n\
             CACHE-CONTROL: max-age=75\r\nLOCATION: http://10.0.0.2:9080/rootDesc.xml\r\n\r\n"
        );
        let byebye =
            String::from_utf8(config().notify(&config().uuid, "ssdp:byebye", None)).unwrap();
        assert!(byebye.contains("USN: uuid:9c75442a-03bc-b28c-d59b-05f614916334\r\n\r\n"));
    }

    #[test]
    fn searches_follow_the_reference_rules() {
        let search = |text: &str| Search::parse(text.as_bytes());
        let request = "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 3\r\nST: ssdp:all\r\n\r\n";
        assert_eq!(
            search(request),
            Some(Search {
                mx: 3,
                target: "ssdp:all".into()
            })
        );
        assert_eq!(search(&request.replace("MX: 3", "MX: 99")).unwrap().mx, 10);
        assert_eq!(search(&request.replace("MX: 3", "MX: x")), None);
        assert_eq!(search(&request.replace("ssdp:discover", "other")), None);
        assert_eq!(
            search(
                &request
                    .replace("HOST: 239.255.255.250:1900", "HOST: x")
                    .replace("MX: 3", "MX: x")
            )
            .unwrap()
            .mx,
            1
        );
        assert_eq!(search("NOTIFY * HTTP/1.1\r\n\r\n"), None);
    }

    #[test]
    fn answers_come_from_addresses_in_the_senders_network() {
        let interface = Interface {
            name: "eth0".into(),
            index: 2,
            up: true,
            loopback: false,
            multicast: true,
            addresses: vec![Address {
                ip: "10.0.0.2".parse().unwrap(),
                prefix: 24,
            }],
        };
        let search = Search {
            mx: 1,
            target: "upnp:rootdevice".into(),
        };
        let answers = search.answers(&config(), &interface, "10.0.0.9".parse().unwrap());
        assert_eq!(answers.len(), 1);
        assert!(answers[0].0 < Duration::from_secs(1));
        assert_eq!(
            String::from_utf8(answers[0].1.clone()).unwrap(),
            "HTTP/1.1 200 OK\r\nCache-Control: max-age=75\r\nExt: \r\n\
             Location: http://10.0.0.2:9080/rootDesc.xml\r\nServer: Linux/3.4 DLNADOC/1.50 UPnP/1.0 dms/1\r\n\
             St: upnp:rootdevice\r\nUsn: uuid:9c75442a-03bc-b28c-d59b-05f614916334::upnp:rootdevice\r\n\
             Content-Length: 0\r\n\r\n"
        );
        assert!(
            search
                .answers(&config(), &interface, "10.0.1.9".parse().unwrap())
                .is_empty()
        );
        let all = Search {
            mx: 1,
            target: "ssdp:all".into(),
        };
        assert_eq!(
            all.answers(&config(), &interface, "10.0.0.9".parse().unwrap())
                .len(),
            6
        );
    }

    #[test]
    fn go_uint_parsing_detects_the_base() {
        assert_eq!(parse_go_uint("10"), Some(10));
        assert_eq!(parse_go_uint("0x1f"), Some(31));
        assert_eq!(parse_go_uint("010"), Some(8));
        assert_eq!(parse_go_uint(""), None);
    }
}
