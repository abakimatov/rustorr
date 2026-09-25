//! `server/netbind`: one server on several bind addresses, as MatriX.145's
//! repeatable `--ip` gives it (each address its own socket, one handler),
//! plus its HTTPS side: `--ssl` serves the same handler over TLS on a second
//! port, and `--force-https` turns the HTTP sockets into redirects.

use std::{future::Future, io, net::SocketAddr, sync::Arc, time::Duration};

use axum::{
    Router,
    body::Body,
    http::{HeaderValue, Request, Response, StatusCode, header},
    serve::ListenerExt,
};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{mpsc, watch},
    task::{AbortHandle, JoinSet},
};
use tokio_rustls::{TlsAcceptor, rustls::ServerConfig, server::TlsStream};

/// A TLS handshake that takes longer is dropped, so a client that connects
/// and says nothing holds no resources.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

enum Kind {
    Plain,
    Tls(TlsAcceptor),
    /// Answers every request with a redirect to HTTPS on this port.
    Redirect(u16),
}

/// The bound sockets. Each gets its own server over one router, as
/// MatriX.145 runs one gin server per address.
pub struct Listeners {
    listeners: Vec<(TcpListener, Kind)>,
}

async fn bind_all(addresses: &[SocketAddr]) -> io::Result<Vec<TcpListener>> {
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
    Ok(listeners)
}

impl Listeners {
    /// # Panics
    ///
    /// Without a listener there is nothing to serve.
    pub fn new(listeners: Vec<TcpListener>) -> Self {
        assert!(!listeners.is_empty(), "at least one listener");
        Self {
            listeners: listeners
                .into_iter()
                .map(|listener| (listener, Kind::Plain))
                .collect(),
        }
    }

    /// Binds every address, failing on the first that cannot be bound, as
    /// `netbind.CheckPort` refuses to start.
    pub async fn bind(addresses: &[SocketAddr]) -> io::Result<Self> {
        Ok(Self::new(bind_all(addresses).await?))
    }

    /// Adds HTTPS sockets on `addresses`, serving the same handler.
    pub async fn bind_tls(
        &mut self,
        addresses: &[SocketAddr],
        config: Arc<ServerConfig>,
    ) -> io::Result<()> {
        let acceptor = TlsAcceptor::from(config);
        for listener in bind_all(addresses).await? {
            self.listeners.push((listener, Kind::Tls(acceptor.clone())));
        }
        Ok(())
    }

    /// `--force-https`: the plain sockets answer with a `307` to the same
    /// path on HTTPS port `https_port`.
    pub fn redirect_to_https(&mut self, https_port: u16) {
        for (_, kind) in &mut self.listeners {
            if matches!(kind, Kind::Plain) {
                *kind = Kind::Redirect(https_port);
            }
        }
    }

    /// The bound addresses of the plain (or redirecting) sockets, in the
    /// order given.
    pub fn local_addrs(&self) -> io::Result<Vec<SocketAddr>> {
        self.listeners
            .iter()
            .filter(|(_, kind)| !matches!(kind, Kind::Tls(_)))
            .map(|(listener, _)| listener.local_addr())
            .collect()
    }

    /// The bound addresses of the HTTPS sockets.
    pub fn tls_addrs(&self) -> io::Result<Vec<SocketAddr>> {
        self.listeners
            .iter()
            .filter(|(_, kind)| matches!(kind, Kind::Tls(_)))
            .map(|(listener, _)| listener.local_addr())
            .collect()
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
        for (listener, kind) in self.listeners {
            let mut stopped = stopped.clone();
            let until_stopped = async move {
                let _ = stopped.wait_for(|stopped| *stopped).await;
            };
            match kind {
                Kind::Plain => {
                    let service = router
                        .clone()
                        .into_make_service_with_connect_info::<SocketAddr>();
                    servers.spawn(async move {
                        axum::serve(listener, service)
                            .with_graceful_shutdown(until_stopped)
                            .await
                    });
                }
                Kind::Redirect(port) => {
                    let service =
                        redirect_router(port).into_make_service_with_connect_info::<SocketAddr>();
                    servers.spawn(async move {
                        axum::serve(listener, service)
                            .with_graceful_shutdown(until_stopped)
                            .await
                    });
                }
                Kind::Tls(acceptor) => {
                    let service = router
                        .clone()
                        .into_make_service_with_connect_info::<SocketAddr>();
                    // `tap_io` makes axum's `ConnectInfo<SocketAddr>` available
                    // for a listener of our own.
                    let listener = TlsListener::new(listener, acceptor).tap_io(|_| {});
                    servers.spawn(async move {
                        axum::serve(listener, service)
                            .with_graceful_shutdown(until_stopped)
                            .await
                    });
                }
            }
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

/// Accepts TCP connections and hands out those whose TLS handshake
/// succeeded. Handshakes run in their own tasks, so one slow client does not
/// hold up the others.
struct TlsListener {
    local: io::Result<SocketAddr>,
    ready: mpsc::Receiver<(TlsStream<TcpStream>, SocketAddr)>,
    acceptor_task: AbortHandle,
}

impl TlsListener {
    fn new(listener: TcpListener, acceptor: TlsAcceptor) -> Self {
        let local = listener.local_addr();
        let (send, ready) = mpsc::channel(64);
        let acceptor_task = tokio::spawn(async move {
            loop {
                let (stream, peer) = match listener.accept().await {
                    Ok(accepted) => accepted,
                    Err(error) => {
                        tracing::debug!(%error, "TLS accept failed");
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        continue;
                    }
                };
                let acceptor = acceptor.clone();
                let send = send.clone();
                tokio::spawn(async move {
                    match tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(stream)).await {
                        Ok(Ok(stream)) => {
                            let _ = send.send((stream, peer)).await;
                        }
                        Ok(Err(error)) => tracing::debug!(%peer, %error, "TLS handshake failed"),
                        Err(_) => tracing::debug!(%peer, "TLS handshake timed out"),
                    }
                });
            }
        })
        .abort_handle();
        Self {
            local,
            ready,
            acceptor_task,
        }
    }
}

impl Drop for TlsListener {
    fn drop(&mut self) {
        self.acceptor_task.abort();
    }
}

impl axum::serve::Listener for TlsListener {
    type Io = TlsStream<TcpStream>;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        match self.ready.recv().await {
            Some(accepted) => accepted,
            // The accepting task only ends when aborted, on drop.
            None => std::future::pending().await,
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        self.local
            .as_ref()
            .map(|address| *address)
            .map_err(|error| io::Error::new(error.kind(), error.to_string()))
    }
}

/// `runHTTPRedirectToHTTPS`: the request's host on the HTTPS port (none for
/// 443), the same path and query. Unlike the reference, which escapes the
/// already escaped path a second time, the path is kept as the client sent
/// it, so names with spaces or Cyrillic survive the redirect.
pub(crate) fn https_target(request: &Request<Body>, https_port: u16) -> String {
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .or_else(|| request.uri().host())
        .unwrap_or("localhost");
    let name = match host.rsplit_once(':') {
        // `[::1]:8090` or `host:8090`; a bare IPv6 address has more colons.
        Some((name, port))
            if port.bytes().all(|byte| byte.is_ascii_digit()) && !name.ends_with(':') =>
        {
            name
        }
        _ => host,
    };
    let authority = if https_port == 443 {
        name.to_string()
    } else {
        format!("{name}:{https_port}")
    };
    let path = match request.uri().path() {
        "" => "/",
        path => path,
    };
    match request.uri().query() {
        Some(query) if !query.is_empty() => format!("https://{authority}{path}?{query}"),
        _ => format!("https://{authority}{path}"),
    }
}

fn redirect_router(https_port: u16) -> Router {
    Router::new().fallback(move |request: Request<Body>| async move {
        let target = https_target(&request, https_port);
        // Go's `http.Redirect`: a short HTML body for GET.
        let body = if request.method() == axum::http::Method::GET {
            format!(
                "<a href=\"{}\">Temporary Redirect</a>.\n\n",
                html_escape(&target)
            )
        } else {
            String::new()
        };
        let mut response = Response::new(Body::from(body));
        *response.status_mut() = StatusCode::TEMPORARY_REDIRECT;
        if let Ok(location) = HeaderValue::from_str(&target) {
            response.headers_mut().insert(header::LOCATION, location);
        }
        if request.method() == axum::http::Method::GET {
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            );
        }
        response
    })
}

fn html_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&#34;")
        .replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpStream,
    };

    use super::*;

    async fn get(address: SocketAddr, path: &str) -> String {
        let mut client = TcpStream::connect(address).await.unwrap();
        client
            .write_all(
                format!(
                    "GET {path} HTTP/1.1\r\nHost: media.local:{}\r\nConnection: close\r\n\r\n",
                    address.port()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).await.unwrap();
        response
    }

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
            let response = get(*address, "/echo").await;
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

    #[tokio::test]
    async fn force_https_redirects_to_the_same_path_on_the_https_port() {
        let mut listeners = Listeners::bind(&["127.0.0.1:0".parse().unwrap()])
            .await
            .unwrap();
        listeners.redirect_to_https(8091);
        let address = listeners.local_addrs().unwrap()[0];
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(listeners.serve(Router::new(), async move {
            let _ = stopped.await;
        }));
        let response = get(address, "/stream/%D0%A4%20x.mkv?link=abc&play").await;
        assert!(response.starts_with("HTTP/1.1 307"), "{response}");
        assert!(
            response
                .contains("location: https://media.local:8091/stream/%D0%A4%20x.mkv?link=abc&play"),
            "{response}"
        );
        stop.send(()).unwrap();
        server.await.unwrap().unwrap();
    }

    #[test]
    fn the_https_target_drops_port_443_and_keeps_ipv6_hosts() {
        let request = |host: &str, uri: &str| {
            Request::builder()
                .uri(uri)
                .header(header::HOST, host)
                .body(Body::empty())
                .unwrap()
        };
        assert_eq!(https_target(&request("tv:8090", "/"), 443), "https://tv/");
        assert_eq!(
            https_target(&request("[::1]:8090", "/echo"), 8091),
            "https://[::1]:8091/echo"
        );
        assert_eq!(
            https_target(&request("tv", "/a?"), 8091),
            "https://tv:8091/a"
        );
    }
}
