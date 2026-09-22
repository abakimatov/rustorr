use std::{any::Any, future::Future, io, sync::Arc};

use axum::{
    Router,
    body::Body,
    extract::{Json, Query, State},
    http::{Request, Response, StatusCode, header},
    middleware::map_response,
    response::IntoResponse,
    routing::{get, post},
};
use rustorr_lifecycle::TorrentCoordinator;
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use tokio_util::io::ReaderStream;
use tower_http::{
    catch_panic::CatchPanicLayer,
    trace::{DefaultOnResponse, TraceLayer},
};
use tracing::{Level, error, info_span};

use crate::ApiError;

/// What the server tells clients about itself.
#[derive(Debug, Clone)]
pub struct ServerInfo {
    pub version: String,
}

#[derive(Clone)]
struct PlaybackState {
    info: ServerInfo,
    torrents: Arc<TorrentCoordinator>,
}

/// The HTTP application.
pub fn router(info: ServerInfo) -> Router {
    with_layers(routes(info))
}

/// R5's deliberately narrow R1 workload surface. R6 replaces this with the
/// contract-complete TorrServer router.
pub fn router_with_lifecycle(info: ServerInfo, torrents: Arc<TorrentCoordinator>) -> Router {
    with_layers(playback_routes(PlaybackState { info, torrents }))
}

/// Serves the application on `listener` until `shutdown` completes, then stops
/// accepting connections and waits for open ones to finish.
///
/// That wait has no limit of its own: a connection that never finishes keeps
/// the future pending. Callers that must stop by a deadline bound it
/// themselves.
pub async fn serve(
    listener: tokio::net::TcpListener,
    info: ServerInfo,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> io::Result<()> {
    axum::serve(listener, router(info))
        .with_graceful_shutdown(shutdown)
        .await
}

pub async fn serve_with_lifecycle(
    listener: tokio::net::TcpListener,
    info: ServerInfo,
    torrents: Arc<TorrentCoordinator>,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> io::Result<()> {
    axum::serve(listener, router_with_lifecycle(info, torrents))
        .with_graceful_shutdown(shutdown)
        .await
}

fn playback_routes(state: PlaybackState) -> Router {
    Router::new()
        .route("/echo", get(echo_playback))
        .route("/torrents", post(torrents))
        .route("/stream", get(stream))
        .with_state(state)
        .fallback(not_found)
        .method_not_allowed_fallback(not_found)
}

fn routes(info: ServerInfo) -> Router {
    Router::new()
        .route("/echo", get(echo))
        .with_state(info)
        .fallback(not_found)
        // The reference has no 405: a known path with the wrong method is a
        // plain 404, exactly like an unknown path (`GET /settings`, R2 corpus).
        .method_not_allowed_fallback(not_found)
}

/// axum adds `Allow` to the response for a wrong method even when the fallback
/// is replaced, and does it after every layer of the router itself has run. The
/// reference's 404 has no such header, so it is removed from outside.
async fn without_allow_on_404(mut response: Response<Body>) -> Response<Body> {
    if response.status() == StatusCode::NOT_FOUND {
        response.headers_mut().remove(header::ALLOW);
    }
    response
}

async fn echo(State(info): State<ServerInfo>) -> String {
    info.version
}

async fn echo_playback(State(state): State<PlaybackState>) -> String {
    state.info.version
}

#[derive(serde::Deserialize)]
struct TorrentAction {
    action: String,
    link: Option<String>,
}

fn lifecycle_error(error: impl std::fmt::Display) -> ApiError {
    ApiError::Json {
        status: StatusCode::BAD_REQUEST,
        message: error.to_string(),
    }
}

async fn torrents(
    State(state): State<PlaybackState>,
    Json(action): Json<TorrentAction>,
) -> Result<Json<Value>, ApiError> {
    match action.action.as_str() {
        "add" => {
            let link = action
                .link
                .ok_or_else(|| lifecycle_error("add requires link"))?;
            let entry = state
                .torrents
                .add_link(&link)
                .await
                .map_err(lifecycle_error)?;
            Ok(Json(json!({"hash": entry.hash.to_string()})))
        }
        "list" => {
            let entries = state.torrents.list().map_err(lifecycle_error)?;
            let mut response = Vec::with_capacity(entries.len());
            for entry in entries {
                let status = state.torrents.status(entry.hash).await;
                response.push(json!({
                    "hash": entry.hash.to_string(),
                    "stat": if status.ready { 3 } else { 0 },
                    "connected_seeders": status.live_peers,
                }));
            }
            Ok(Json(Value::Array(response)))
        }
        "wipe" => {
            for entry in state.torrents.list().map_err(lifecycle_error)? {
                state
                    .torrents
                    .drop_torrent(entry.hash)
                    .await
                    .map_err(lifecycle_error)?;
            }
            Ok(Json(Value::Null))
        }
        _ => Err(lifecycle_error("unsupported R5 torrent action")),
    }
}

#[derive(serde::Deserialize)]
struct StreamQuery {
    link: String,
    index: u32,
}

fn requested_range(
    value: Option<&axum::http::HeaderValue>,
) -> Result<(u64, Option<u64>), ApiError> {
    let Some(value) = value else {
        return Ok((0, None));
    };
    let text = value
        .to_str()
        .map_err(|_| lifecycle_error("invalid Range header"))?;
    let bytes = text
        .strip_prefix("bytes=")
        .ok_or_else(|| lifecycle_error("only bytes ranges are supported"))?;
    let (start, end) = bytes
        .split_once('-')
        .ok_or_else(|| lifecycle_error("invalid Range header"))?;
    if start.is_empty() || bytes.contains(',') {
        return Err(lifecycle_error("invalid Range header"));
    }
    let start = start
        .parse()
        .map_err(|_| lifecycle_error("invalid Range start"))?;
    let end = (!end.is_empty())
        .then(|| end.parse())
        .transpose()
        .map_err(|_| lifecycle_error("invalid Range end"))?;
    Ok((start, end))
}

async fn stream(
    State(state): State<PlaybackState>,
    Query(query): Query<StreamQuery>,
    request: Request<Body>,
) -> Result<Response<Body>, ApiError> {
    let (start, requested_end) = requested_range(request.headers().get(header::RANGE))?;
    let entry = state
        .torrents
        .add_link(&query.link)
        .await
        .map_err(lifecycle_error)?;
    let reader = state
        .torrents
        .play(entry.hash, query.index, start)
        .await
        .map_err(lifecycle_error)?;
    let length = reader.file_length();
    if start >= length {
        return Err(ApiError::Status(StatusCode::RANGE_NOT_SATISFIABLE));
    }
    let end = requested_end.unwrap_or(length - 1).min(length - 1);
    if end < start {
        return Err(ApiError::Status(StatusCode::RANGE_NOT_SATISFIABLE));
    }
    let body_length = end - start + 1;
    state
        .torrents
        .schedule_prefetch(entry.hash, query.index, end.saturating_add(1))
        .await;
    let body = Body::from_stream(ReaderStream::new(reader.take(body_length)));
    Response::builder()
        .status(StatusCode::PARTIAL_CONTENT)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_LENGTH, body_length)
        .header(
            header::CONTENT_RANGE,
            format!("bytes {start}-{end}/{length}"),
        )
        .body(body)
        .map_err(|_| ApiError::Status(StatusCode::INTERNAL_SERVER_ERROR))
}

async fn not_found() -> ApiError {
    ApiError::NotFound
}

/// Wraps routes in the layers every request passes through.
///
/// Order, outermost first: tracing, then (added in R6) CORS, WAF and auth, then
/// panic recovery. Recovery is innermost so a panicking handler still yields a
/// response that the access layers and the trace log see as a normal `500`.
///
/// The routes are mounted as the fallback of an outer router because only
/// layers of an outer router see the response after axum has finished with it.
pub(crate) fn with_layers(routes: Router) -> Router {
    let routes = routes.layer(CatchPanicLayer::custom(panicked));
    Router::new()
        .fallback_service(routes)
        .layer(map_response(without_allow_on_404))
        .layer(
            TraceLayer::new_for_http()
                // The path only: torrent links carry tracker URLs with
                // credentials, so the query string must never reach a log.
                .make_span_with(|request: &Request<Body>| {
                    info_span!(
                        "request",
                        method = %request.method(),
                        path = %request.uri().path()
                    )
                })
                .on_response(DefaultOnResponse::new().level(Level::INFO)),
        )
}

fn panicked(payload: Box<dyn Any + Send + 'static>) -> Response<Body> {
    let message = payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("non-string panic payload");
    error!(panic = %message, "request handler panicked");
    ApiError::Status(StatusCode::INTERNAL_SERVER_ERROR).into_response()
}

#[cfg(test)]
// The serial lock is held across awaits on purpose: it serializes whole tests.
#[allow(clippy::await_holding_lock)]
mod tests {
    use std::{
        io,
        sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError},
    };

    use axum::body::to_bytes;
    use tower::ServiceExt;
    use tracing_subscriber::fmt::MakeWriter;

    use super::*;

    fn app() -> Router {
        router(ServerInfo {
            version: "rustorr 9.9.9".into(),
        })
    }

    async fn call(
        app: Router,
        method: &str,
        uri: &str,
    ) -> (StatusCode, Vec<(String, String)>, Vec<u8>) {
        let request = Request::builder()
            .method(method)
            .uri(uri)
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response
            .headers()
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_str().unwrap().to_owned()))
            .collect();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, headers, body.to_vec())
    }

    fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
        headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    #[derive(Clone, Default)]
    struct Log(Arc<Mutex<Vec<u8>>>);

    impl io::Write for Log {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for Log {
        type Writer = Log;

        fn make_writer(&'a self) -> Log {
            self.clone()
        }
    }

    impl Log {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }

        fn clear(&self) {
            self.0.lock().unwrap().clear();
        }
    }

    static SERIAL: Mutex<()> = Mutex::new(());
    static LOG: OnceLock<Log> = OnceLock::new();

    /// Every test that sends a request holds this for its whole body. The tests
    /// share one global log, so what a log test reads must come from that test
    /// alone. Thread-local subscribers per test are not an option: with tests
    /// running in parallel, `tracing` intermittently dropped events (about one
    /// run in seven).
    fn serial() -> MutexGuard<'static, ()> {
        SERIAL.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The shared log, emptied. Call it only while holding [`serial`].
    fn logs() -> Log {
        let log = LOG.get_or_init(|| {
            let log = Log::default();
            tracing_subscriber::fmt()
                .with_writer(log.clone())
                .with_ansi(false)
                .with_max_level(Level::INFO)
                .init();
            log
        });
        log.clear();
        log.clone()
    }

    async fn panics() -> &'static str {
        panic!("boom")
    }

    #[test]
    fn r5_range_parser_accepts_closed_and_open_ended_ranges() {
        assert_eq!(
            requested_range(Some(&header::HeaderValue::from_static("bytes=10-99"))).unwrap(),
            (10, Some(99))
        );
        assert_eq!(
            requested_range(Some(&header::HeaderValue::from_static("bytes=10-"))).unwrap(),
            (10, None)
        );
        assert_eq!(requested_range(None).unwrap(), (0, None));
    }

    #[test]
    fn r5_range_parser_rejects_suffix_and_multi_ranges() {
        for value in ["bytes=-99", "bytes=0-1,4-5", "items=0-1", "bytes=bad-1"] {
            assert!(requested_range(Some(&header::HeaderValue::from_static(value))).is_err());
        }
    }

    fn panicking_app() -> Router {
        with_layers(
            Router::new()
                .route("/panic", get(panics))
                .route("/ok", get(|| async { "fine" })),
        )
    }

    async fn raw_get(address: std::net::SocketAddr, path: &str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
        let request = format!("GET {path} HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n");
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        response
    }

    #[tokio::test]
    async fn serve_answers_over_tcp_and_stops_when_told_to() {
        let _serial = serial();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(serve(
            listener,
            ServerInfo {
                version: "rustorr 9.9.9".into(),
            },
            async {
                let _ = stopped.await;
            },
        ));

        let response = raw_get(address, "/echo").await;
        assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
        assert!(response.ends_with("rustorr 9.9.9"), "{response}");

        stop.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), server)
            .await
            .expect("the server must stop once told to and idle")
            .unwrap()
            .unwrap();
        assert!(tokio::net::TcpStream::connect(address).await.is_err());
    }

    #[tokio::test]
    async fn echo_returns_the_configured_version() {
        let _serial = serial();
        let (status, headers, body) = call(app(), "GET", "/echo").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, b"rustorr 9.9.9");
        assert_eq!(
            header(&headers, "content-type"),
            Some("text/plain; charset=utf-8")
        );
    }

    #[tokio::test]
    async fn an_unknown_path_is_the_reference_404() {
        let _serial = serial();
        let (status, headers, body) = call(app(), "GET", "/no/such/route").await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body, b"404 page not found");
        assert_eq!(header(&headers, "content-type"), Some("text/plain"));
        assert_eq!(header(&headers, "content-length"), Some("18"));
    }

    #[tokio::test]
    async fn the_wrong_method_on_a_known_path_is_a_404_not_a_405() {
        let _serial = serial();
        for method in ["POST", "PUT", "DELETE", "PATCH"] {
            let (status, headers, body) = call(app(), method, "/echo").await;

            assert_eq!(status, StatusCode::NOT_FOUND, "{method}");
            assert_eq!(body, b"404 page not found", "{method}");
            assert_eq!(
                header(&headers, "allow"),
                None,
                "{method} must not advertise methods"
            );
        }
    }

    #[tokio::test]
    async fn a_panicking_handler_becomes_an_empty_500_and_the_app_keeps_serving() {
        let _serial = serial();
        let app = panicking_app();

        let (status, _, body) = call(app.clone(), "GET", "/panic").await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(body.is_empty());

        let (status, _, body) = call(app, "GET", "/ok").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, b"fine");
    }

    #[tokio::test]
    async fn requests_are_logged_with_method_path_and_status() {
        let _serial = serial();
        let log = logs();

        call(app(), "GET", "/echo").await;

        let text = log.text();
        assert!(text.contains("method=GET"), "{text}");
        assert!(text.contains("path=/echo"), "{text}");
        assert!(text.contains("status=200"), "{text}");
    }

    #[tokio::test]
    async fn the_query_string_never_reaches_the_log() {
        let _serial = serial();
        let log = logs();

        call(
            app(),
            "GET",
            "/echo?link=magnet:?xt=urn:btih:abc%26tr=http://t/announce?passkey=SECRET",
        )
        .await;

        let text = log.text();
        assert!(text.contains("path=/echo"), "{text}");
        for leaked in ["SECRET", "passkey", "magnet", "link="] {
            assert!(
                !text.contains(leaked),
                "{leaked} leaked into the log: {text}"
            );
        }
    }

    #[tokio::test]
    async fn a_panic_is_logged_with_its_message_and_the_500_is_traced() {
        let _serial = serial();
        let log = logs();

        call(panicking_app(), "GET", "/panic").await;

        let text = log.text();
        assert!(text.contains("request handler panicked"), "{text}");
        assert!(text.contains("boom"), "{text}");
        assert!(text.contains("status=500"), "{text}");
    }
}
