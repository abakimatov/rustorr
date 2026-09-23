//! Multicast UDP sockets shared by the mDNS responder and the SSDP server.

use std::{
    io,
    net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4},
};

use socket2::{Domain, Protocol, SockRef, Socket, Type};
use tokio::{net::UdpSocket, sync::Mutex};

use crate::interfaces::Interface;

/// A socket bound to a multicast port and joined to its group on chosen
/// interfaces. Sends name their interface; the lock keeps the choice of
/// interface and the send together.
pub(crate) struct MulticastSocket {
    socket: UdpSocket,
    send: Mutex<()>,
}

impl MulticastSocket {
    /// Binds `bind:port` with address reuse and joins `group` on every
    /// interface that accepts it. Fails when no interface does.
    pub(crate) fn open(
        bind: Ipv4Addr,
        group: Ipv4Addr,
        port: u16,
        interfaces: &[Interface],
        ttl: Option<u32>,
    ) -> io::Result<Self> {
        let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
        socket.set_reuse_address(true)?;
        #[cfg(not(target_os = "windows"))]
        socket.set_reuse_port(true)?;
        socket.bind(&SocketAddr::V4(SocketAddrV4::new(bind, port)).into())?;
        let mut joined = 0;
        for interface in interfaces {
            let membership = socket2::InterfaceIndexOrAddress::Index(interface.index);
            if socket.join_multicast_v4_n(&group, &membership).is_ok() {
                joined += 1;
            }
        }
        if joined == 0 && !interfaces.is_empty() {
            return Err(io::Error::other(
                "cannot join the multicast group on any interface",
            ));
        }
        if let Some(ttl) = ttl {
            socket.set_multicast_ttl_v4(ttl)?;
        }
        socket.set_nonblocking(true)?;
        Ok(Self {
            socket: UdpSocket::from_std(socket.into())?,
            send: Mutex::new(()),
        })
    }

    pub(crate) async fn recv_from(&self, buffer: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        self.socket.recv_from(buffer).await
    }

    /// Sends through `interface`, or by the routing table without one.
    pub(crate) async fn send(
        &self,
        bytes: &[u8],
        to: SocketAddr,
        interface: Option<&Interface>,
    ) -> io::Result<()> {
        let _guard = self.send.lock().await;
        if let Some(interface) = interface {
            let address = interface
                .addresses
                .iter()
                .find_map(|address| match address.ip {
                    IpAddr::V4(v4) => Some(v4),
                    IpAddr::V6(_) => None,
                })
                .unwrap_or(Ipv4Addr::UNSPECIFIED);
            SockRef::from(&self.socket).set_multicast_if_v4(&address)?;
        }
        self.socket.send_to(bytes, to).await.map(|_| ())
    }
}
