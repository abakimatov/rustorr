//! MatriX.145's Bonjour advertisement (`server/bonjour` over
//! `grandcat/zeroconf`): `_torrserver._tcp` and `_http._tcp` answered and
//! announced like zeroconf's `RegisterProxy` servers — one responder per
//! service, two probes and two announcements on start, goodbye records on
//! stop.

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4},
    sync::Arc,
    time::Duration,
};

use rand::Rng;
use tokio::{sync::Mutex, task::JoinHandle};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::{
    dns::{CACHE_FLUSH, CLASS_IN, Data, Message, Question, Record, TYPE_PTR},
    identity,
    interfaces::{self, Interface},
    socket::MulticastSocket,
};

const GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);
const PORT: u16 = 5353;
const TTL: u32 = 3200;
const ADDRESS_TTL: u32 = 120;
const SERVICES: [&str; 2] = ["_torrserver._tcp", "_http._tcp"];
/// Interface name prefixes the reference never advertises on: tunnels,
/// bridges and container plumbing.
const SKIPPED_PREFIXES: [&str; 14] = [
    "utun", "awdl", "llw", "ap", "bridge", "anpi", "gif", "stf", "vmnet", "veth", "docker", "br-",
    "cni", "flannel",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BonjourConfig {
    /// `FriendlyName` from the settings; empty means `TorrServer`.
    pub friendly_name: String,
    /// The web port.
    pub port: u16,
    pub version: String,
}

/// Starts and stops the advertisement as settings change.
#[derive(Default)]
pub struct Bonjour {
    running: Mutex<Vec<Responder>>,
}

impl Bonjour {
    pub fn new() -> Self {
        Self::default()
    }

    /// `bonjour.Start`: replaces any running advertisement.
    pub async fn start(&self, config: BonjourConfig) {
        let mut running = self.running.lock().await;
        stop_all(&mut running).await;
        if config.port == 0 {
            warn!(port = config.port, "Bonjour: invalid web port");
            return;
        }
        let (interfaces, addresses) = advertised(interfaces::list());
        if addresses.is_empty() {
            warn!("Bonjour: no suitable interface addresses");
            return;
        }
        let host = format!(
            "{}.local.",
            identity::bonjour_host(identity::hostname().as_deref())
        );
        let instance = String::from_utf8_lossy(&identity::bonjour_instance(&config.friendly_name))
            .into_owned();
        for service in SERVICES {
            let records = Service {
                type_name: "_services._dns-sd._udp.local.".into(),
                service_name: format!("{service}.local."),
                instance_name: format!("{instance}.{service}.local."),
                host: host.clone(),
                port: config.port,
                text: vec![format!("version={}", config.version), "path=/".into()],
                addresses: addresses.clone(),
            };
            match Responder::start(records, interfaces.clone()) {
                Ok(responder) => {
                    info!(%instance, service, %host, port = config.port, "Bonjour: advertising");
                    running.push(responder);
                }
                Err(error) => warn!(service, %error, "Bonjour: register failed"),
            }
        }
    }

    /// `bonjour.Stop`: goodbye records for every service, then silence.
    pub async fn stop(&self) {
        stop_all(&mut *self.running.lock().await).await;
    }
}

async fn stop_all(running: &mut Vec<Responder>) {
    if running.is_empty() {
        return;
    }
    for responder in running.drain(..) {
        responder.stop().await;
    }
    info!("Bonjour: stopped");
}

/// `advertiseAddrs`: usable interfaces and their addresses, IPv4 before IPv6
/// within each interface, without loopback or link-local ones.
fn advertised(all: Vec<Interface>) -> (Vec<Interface>, Vec<IpAddr>) {
    let mut interfaces = Vec::new();
    let mut addresses: Vec<IpAddr> = Vec::new();
    for interface in all {
        let name = interface.name.to_lowercase();
        if interface.loopback
            || !interface.up
            || !interface.multicast
            || SKIPPED_PREFIXES
                .iter()
                .any(|prefix| name.starts_with(prefix))
        {
            continue;
        }
        let usable: Vec<IpAddr> = interface
            .addresses
            .iter()
            .map(|address| address.ip)
            .filter(|ip| !ip.is_loopback() && !interfaces::link_local(*ip))
            .collect();
        if usable.is_empty() {
            continue;
        }
        let (v4, v6): (Vec<IpAddr>, Vec<IpAddr>) = usable.into_iter().partition(IpAddr::is_ipv4);
        for ip in v4.into_iter().chain(v6) {
            if !addresses.contains(&ip) {
                addresses.push(ip);
            }
        }
        interfaces.push(interface);
    }
    (interfaces, addresses)
}

/// One advertised service's names and data.
#[derive(Debug, Clone)]
struct Service {
    type_name: String,
    service_name: String,
    instance_name: String,
    host: String,
    port: u16,
    text: Vec<String>,
    addresses: Vec<IpAddr>,
}

impl Service {
    fn record(&self, name: &str, class: u16, ttl: u32, data: Data) -> Record {
        Record {
            name: name.into(),
            class,
            ttl,
            data,
        }
    }

    fn ptr(&self, ttl: u32) -> Record {
        self.record(
            &self.service_name,
            CLASS_IN,
            ttl,
            Data::Ptr(self.instance_name.clone()),
        )
    }

    fn dnssd(&self, ttl: u32) -> Record {
        self.record(
            &self.type_name,
            CLASS_IN,
            ttl,
            Data::Ptr(self.service_name.clone()),
        )
    }

    fn srv(&self, class: u16, ttl: u32) -> Record {
        self.record(
            &self.instance_name,
            class,
            ttl,
            Data::Srv {
                port: self.port,
                target: self.host.clone(),
            },
        )
    }

    fn txt(&self, class: u16, ttl: u32) -> Record {
        self.record(
            &self.instance_name,
            class,
            ttl,
            Data::Txt(self.text.clone()),
        )
    }

    /// `appendAddrs`: IPv4 first; any positive TTL becomes 120.
    fn addresses(&self, ttl: u32, flush: bool) -> Vec<Record> {
        let ttl = if ttl > 0 { ADDRESS_TTL } else { 0 };
        let class = if flush {
            CLASS_IN | CACHE_FLUSH
        } else {
            CLASS_IN
        };
        let v4 = self.addresses.iter().filter_map(|ip| match ip {
            IpAddr::V4(v4) => Some(Data::A(*v4)),
            IpAddr::V6(_) => None,
        });
        let v6 = self.addresses.iter().filter_map(|ip| match ip {
            IpAddr::V6(v6) => Some(Data::Aaaa(*v6)),
            IpAddr::V4(_) => None,
        });
        v4.chain(v6)
            .map(|data| self.record(&self.host, class, ttl, data))
            .collect()
    }

    /// `composeLookupAnswers`.
    fn lookup(&self, ttl: u32, flush: bool) -> Vec<Record> {
        let mut answers = vec![
            self.srv(CLASS_IN | CACHE_FLUSH, ttl),
            self.txt(CLASS_IN | CACHE_FLUSH, ttl),
            self.ptr(ttl),
            self.dnssd(ttl),
        ];
        answers.extend(self.addresses(ttl, flush));
        answers
    }

    /// `handleQuestion` for one question: the answer and additional records,
    /// or nothing when the name is not this service's.
    fn answer(&self, question: &Question, query: &Message) -> Option<(Vec<Record>, Vec<Record>)> {
        let (answers, additional) = if question.name == self.type_name {
            (vec![self.dnssd(TTL)], Vec::new())
        } else if question.name == self.service_name {
            let mut additional = vec![self.srv(CLASS_IN, TTL), self.txt(CLASS_IN, TTL)];
            additional.extend(self.addresses(TTL, false));
            (vec![self.ptr(TTL)], additional)
        } else if question.name == self.instance_name {
            return Some((self.lookup(TTL, false), Vec::new()));
        } else {
            return None;
        };
        // `isKnownAnswer`: the querier already holds this PTR with at least
        // half its lifetime left.
        let Data::Ptr(target) = &answers[0].data else {
            return None;
        };
        let known = query.answers.iter().any(|known| {
            matches!(&known.data, Data::Ptr(held) if held == target) && known.ttl >= TTL / 2
        });
        (!known).then_some((answers, additional))
    }
}

struct Responder {
    stop: CancellationToken,
    tasks: Vec<JoinHandle<()>>,
    socket: Arc<MulticastSocket>,
    service: Arc<Service>,
    interfaces: Arc<Vec<Interface>>,
}

impl Responder {
    fn start(service: Service, interfaces: Vec<Interface>) -> std::io::Result<Self> {
        let socket = Arc::new(MulticastSocket::open(
            Ipv4Addr::UNSPECIFIED,
            GROUP,
            PORT,
            &interfaces,
            None,
        )?);
        let service = Arc::new(service);
        let interfaces = Arc::new(interfaces);
        let stop = CancellationToken::new();
        let receiver = tokio::spawn(receive(
            Arc::clone(&socket),
            Arc::clone(&service),
            Arc::clone(&interfaces),
            stop.clone(),
        ));
        let announcer = tokio::spawn(announce(
            Arc::clone(&socket),
            Arc::clone(&service),
            Arc::clone(&interfaces),
            stop.clone(),
        ));
        Ok(Self {
            stop,
            tasks: vec![receiver, announcer],
            socket,
            service,
            interfaces,
        })
    }

    /// `unregister`: every record with a zero TTL on every interface.
    async fn stop(self) {
        self.stop.cancel();
        for task in self.tasks {
            let _ = task.await;
        }
        let goodbye = Message {
            response: true,
            answers: self.service.lookup(0, true),
            ..Message::default()
        };
        multicast(&self.socket, &goodbye.pack(false), &self.interfaces).await;
    }
}

async fn multicast(socket: &MulticastSocket, bytes: &[u8], interfaces: &[Interface]) {
    let group = SocketAddr::V4(SocketAddrV4::new(GROUP, PORT));
    for interface in interfaces {
        let _ = socket.send(bytes, group, Some(interface)).await;
    }
}

/// `probe`: two probe queries, then an announcement on each interface one
/// and three seconds later.
async fn announce(
    socket: Arc<MulticastSocket>,
    service: Arc<Service>,
    interfaces: Arc<Vec<Interface>>,
    stop: CancellationToken,
) {
    let probe = Message {
        id: rand::rng().random(),
        questions: vec![Question {
            name: service.instance_name.clone(),
            kind: TYPE_PTR,
            class: CLASS_IN,
        }],
        authority: vec![service.srv(CLASS_IN, TTL), service.txt(CLASS_IN, TTL)],
        ..Message::default()
    }
    .pack(false);
    for _ in 0..2 {
        multicast(&socket, &probe, &interfaces).await;
        let pause = Duration::from_millis(rand::rng().random_range(0..250));
        if sleep_or_stop(pause, &stop).await {
            return;
        }
    }
    let mut pause = Duration::from_secs(1);
    for _ in 0..2 {
        for interface in interfaces.iter() {
            let announcement = Message {
                response: true,
                answers: service.lookup(TTL, true),
                ..Message::default()
            };
            let group = SocketAddr::V4(SocketAddrV4::new(GROUP, PORT));
            let _ = socket
                .send(&announcement.pack(true), group, Some(interface))
                .await;
        }
        if sleep_or_stop(pause, &stop).await {
            return;
        }
        pause *= 2;
    }
}

async fn sleep_or_stop(pause: Duration, stop: &CancellationToken) -> bool {
    tokio::select! {
        () = stop.cancelled() => true,
        () = tokio::time::sleep(pause) => false,
    }
}

async fn receive(
    socket: Arc<MulticastSocket>,
    service: Arc<Service>,
    interfaces: Arc<Vec<Interface>>,
    stop: CancellationToken,
) {
    let mut buffer = vec![0; 65536];
    loop {
        let received = tokio::select! {
            () = stop.cancelled() => return,
            received = socket.recv_from(&mut buffer) => received,
        };
        let Ok((length, from)) = received else {
            continue;
        };
        let Some(query) = Message::parse(&buffer[..length]) else {
            continue;
        };
        // Probes from other responders carry authority records.
        if !query.authority.is_empty() {
            continue;
        }
        for question in &query.questions {
            let Some((answers, additional)) = service.answer(question, &query) else {
                continue;
            };
            let response = Message {
                id: query.id,
                response: true,
                opcode: query.opcode,
                authoritative: true,
                checking_disabled: query.checking_disabled,
                answers,
                additional,
                ..Message::default()
            }
            .pack(true);
            if question.class & CACHE_FLUSH != 0 {
                let _ = socket.send(&response, from, None).await;
                continue;
            }
            // The reference answers on the interface the query came in on.
            let arrived: Vec<Interface> = interfaces
                .iter()
                .filter(|interface| {
                    interface
                        .addresses
                        .iter()
                        .any(|address| address.contains(from.ip()))
                })
                .cloned()
                .collect();
            let targets = if arrived.is_empty() {
                interfaces.as_slice()
            } else {
                &arrived
            };
            multicast(&socket, &response, targets).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interfaces::Address;

    fn service() -> Service {
        Service {
            type_name: "_services._dns-sd._udp.local.".into(),
            service_name: "_torrserver._tcp.local.".into(),
            instance_name: "TorrServer._torrserver._tcp.local.".into(),
            host: "box.local.".into(),
            port: 8090,
            text: vec!["version=MatriX.145".into(), "path=/".into()],
            addresses: vec!["10.0.0.2".parse().unwrap(), "fd00::2".parse().unwrap()],
        }
    }

    fn question(name: &str) -> Question {
        Question {
            name: name.into(),
            kind: TYPE_PTR,
            class: CLASS_IN,
        }
    }

    #[test]
    fn browsing_answers_put_the_service_records_in_the_additional_section() {
        let service = service();
        let (answers, additional) = service
            .answer(&question("_torrserver._tcp.local."), &Message::default())
            .unwrap();
        assert_eq!(answers, vec![service.ptr(TTL)]);
        let kinds: Vec<(u16, u16, u32)> = additional
            .iter()
            .map(|record| (record.kind(), record.class, record.ttl))
            .collect();
        assert_eq!(
            kinds,
            vec![(33, 1, 3200), (16, 1, 3200), (1, 1, 120), (28, 1, 120)]
        );
    }

    #[test]
    fn instance_lookups_flush_srv_and_txt_only() {
        let (answers, additional) = service()
            .answer(
                &question("TorrServer._torrserver._tcp.local."),
                &Message::default(),
            )
            .unwrap();
        assert!(additional.is_empty());
        let classes: Vec<u16> = answers.iter().map(|record| record.class).collect();
        assert_eq!(
            classes,
            vec![CLASS_IN | CACHE_FLUSH, CLASS_IN | CACHE_FLUSH, 1, 1, 1, 1]
        );
    }

    #[test]
    fn known_answers_and_other_names_are_not_answered() {
        let service = service();
        let known = Message {
            answers: vec![service.ptr(TTL)],
            ..Message::default()
        };
        assert!(
            service
                .answer(&question("_torrserver._tcp.local."), &known)
                .is_none()
        );
        assert!(
            service
                .answer(&question("_other._tcp.local."), &Message::default())
                .is_none()
        );
        // Names compare case-sensitively, as in zeroconf.
        assert!(
            service
                .answer(&question("_TorrServer._tcp.local."), &Message::default())
                .is_none()
        );
    }

    #[test]
    fn goodbyes_carry_zero_ttls_and_flush_addresses() {
        let goodbye = service().lookup(0, true);
        assert!(goodbye.iter().all(|record| record.ttl == 0));
        assert!(goodbye[4].class & CACHE_FLUSH != 0);
    }

    #[test]
    fn advertised_addresses_skip_container_links_loopback_and_link_local() {
        let interface = |name: &str, ip: &str| Interface {
            name: name.into(),
            index: 1,
            up: true,
            loopback: false,
            multicast: true,
            addresses: vec![Address {
                ip: ip.parse().unwrap(),
                prefix: 24,
            }],
        };
        let (interfaces, addresses) = advertised(vec![
            interface("eth0", "172.31.252.4"),
            interface("docker0", "172.17.0.1"),
            interface("eth1", "169.254.1.1"),
        ]);
        assert_eq!(interfaces.len(), 1);
        assert_eq!(addresses, vec!["172.31.252.4".parse::<IpAddr>().unwrap()]);
    }
}
