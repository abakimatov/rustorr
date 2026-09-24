//! `server/netbind`: one server on several bind addresses, as MatriX.145's
//! repeatable `--ip` gives it (each address its own socket, one handler).

use std::{future::Future, io, net::SocketAddr};

use axum::Router;
use tokio::{net::TcpListener, sync::watch, task::JoinSet};

/// The bound sockets. Each gets its own server over one router, as
/// MatriX.145 runs one gin server per address.
pub struct Listeners {
    listeners: Vec<TcpListener>,
}

impl Listeners {
    /// # Panics
    ///
    /// Without a listener there is nothing to serve.
    pub fn new(listeners: Vec<TcpListener>) -> Self {
        assert!(!listeners.is_empty(), "at least one listener");
        Self { listeners }
    }

    /// Binds every address, failing on the first that cannot be bound, as
    /// `netbind.CheckPort` refuses to start.
    pub async fn bind(addresses: &[SocketAddr]) -> io::Result<Self> {
        let mut listeners = Vec::with_capacity(addresses.len());
        for (index, address) in addresses.iter().enumerate() {
            // `netbind.Normalize`: a repeated address is bound once.
            if addresses[..index].contains(address) {
                continue;
            }
            let listener = TcpListener::bind(address)
                .await
                .map_err(|error| io::Error::new(error.kind(), format!("{address}: {error}")))?;
            listeners.push(listener);
        }
        if listeners.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "no address to listen on",
            ));
        }
        Ok(Self::new(listeners))
    }

    /// The bound addresses, in the order given.
    pub fn local_addrs(&self) -> io::Result<Vec<SocketAddr>> {
        self.listeners.iter().map(TcpListener::local_addr).collect()
    }

    /// Serves `router` on every socket until `shutdown`, each server
    /// finishing its connections; the first error wins.
    pub async fn serve(
        self,
        router: Router,
        shutdown: impl Future<Output = ()> + Send + 'static,
    ) -> io::Result<()> {
        let (stop, stopped) = watch::channel(false);
        tokio::spawn(async move {
            shutdown.await;
            let _ = stop.send(true);
        });
        let mut servers = JoinSet::new();
        for listener in self.listeners {
            let mut stopped = stopped.clone();
            let service = router
                .clone()
                .into_make_service_with_connect_info::<SocketAddr>();
            servers.spawn(async move {
                axum::serve(listener, service)
                    .with_graceful_shutdown(async move {
                        let _ = stopped.wait_for(|stopped| *stopped).await;
                    })
                    .await
            });
        }
        let mut outcome = Ok(());
        while let Some(result) = servers.join_next().await {
            let result = result.unwrap_or_else(|error| Err(io::Error::other(error)));
            if outcome.is_ok() {
                outcome = result;
            }
        }
        outcome
    }
}

#[cfg(test)]
mod tests {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpStream,
    };

    use super::*;

    #[tokio::test]
    async fn every_address_serves_the_router_until_shutdown() {
        // A repeated address is bound once.
        let listeners = Listeners::bind(&[
            "127.0.0.1:0".parse().unwrap(),
            "127.0.0.2:0".parse().unwrap(),
            "127.0.0.1:0".parse().unwrap(),
        ])
        .await
        .unwrap();
        let addresses = listeners.local_addrs().unwrap();
        assert_eq!(addresses.len(), 2);
        let router = Router::new().route("/echo", axum::routing::get(|| async { "ok" }));
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(listeners.serve(router, async move {
            let _ = stopped.await;
        }));
        for address in &addresses {
            let mut client = TcpStream::connect(address).await.unwrap();
            client
                .write_all(b"GET /echo HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
            let mut response = String::new();
            client.read_to_string(&mut response).await.unwrap();
            assert!(response.ends_with("ok"), "{response}");
        }
        stop.send(()).unwrap();
        server.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn an_address_in_use_fails_the_whole_bind() {
        let taken = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = taken.local_addr().unwrap();
        let error = Listeners::bind(&["127.0.0.1:0".parse().unwrap(), address])
            .await
            .err()
            .unwrap();
        assert!(
            error.to_string().starts_with(&address.to_string()),
            "{error}"
        );
    }
}
