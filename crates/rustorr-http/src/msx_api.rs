//! MatriX.145's Media Station X integration (`server/web/msx`): the MSX
//! start document, torrent status labels, an outbound proxy for the MSX
//! front end, IMDb poster lookup and `/files`, a link to a local media
//! directory served like `http.FileServer`.

use std::{
    io,
    path::{Path, PathBuf},
    sync::Mutex,
};

use axum::{
    body::{Body, HttpBody, to_bytes},
    extract::{Path as UrlPath, State},
    http::{HeaderMap, HeaderName, HeaderValue, Method, Request, Response, StatusCode, header},
    response::IntoResponse,
};
use rustorr_lifecycle::{InfoHash, TorrentCommand, TorrentReply};
use serde::Serialize;
use serde_json::{Map, Value};

use crate::{
    ApiError,
    app::{AppState, json_response, lifecycle, management_authorized, peer, unauthorized},
    file_server::{self, FileRequest, html_escape},
    go_decode,
};

/// The MSX front end the reference proxies at `/msx/`.
pub const MSX_LANDING_URL: &str = "http://tsmsx.yourok.ru";
const IMDB_SUGGESTIONS_URL: &str = "https://v2.sg.media-imdb.com/suggestion/h/";
const DEFAULT_PARAMETER: &str =
    "menu:request:interaction:{SERVER}@{PREFIX}tsmsx.yourok.ru/start.html";
/// Bodies read whole: `POST /msx/start.json`, `/msx/trn` and `/files`.
const BODY_LIMIT: usize = 4 << 20;

/// State of the MSX integration. The start parameter lives in memory only,
/// as in the reference.
pub struct Msx {
    client: reqwest::Client,
    /// `<data-dir>/media`, the symbolic link `/files` manages.
    media: Option<PathBuf>,
    landing_url: String,
    imdb_url: String,
    parameter: Mutex<String>,
}

impl Msx {
    /// `client` is the server's shared outbound client.
    pub fn new(client: reqwest::Client, data_dir: &Path) -> Self {
        Self::with_endpoints(
            client,
            Some(data_dir),
            MSX_LANDING_URL.into(),
            IMDB_SUGGESTIONS_URL.into(),
        )
    }

    /// No data directory: `/files` shows no link and cannot set one.
    pub(crate) fn detached() -> Self {
        Self::with_endpoints(
            reqwest::Client::new(),
            None,
            MSX_LANDING_URL.into(),
            IMDB_SUGGESTIONS_URL.into(),
        )
    }

    fn with_endpoints(
        client: reqwest::Client,
        data_dir: Option<&Path>,
        landing_url: String,
        imdb_url: String,
    ) -> Self {
        Self {
            client,
            media: data_dir.map(|directory| directory.join("media")),
            landing_url,
            imdb_url,
            parameter: Mutex::new(DEFAULT_PARAMETER.into()),
        }
    }

    fn parameter(&self) -> String {
        self.parameter
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn set_parameter(&self, parameter: String) {
        *self
            .parameter
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = parameter;
    }
}

async fn body_bytes(request: Request<Body>) -> Result<Vec<u8>, Response<Body>> {
    to_bytes(request.into_body(), BODY_LIMIT)
        .await
        .map(|bytes| bytes.to_vec())
        .map_err(|_| ApiError::Status(StatusCode::BAD_REQUEST).into_response())
}

/// gin's `c.Query`: the first value of a query parameter.
fn first_query(request: &Request<Body>, name: &str) -> String {
    query_values(request, name)
        .into_iter()
        .next()
        .unwrap_or_default()
}

fn query_values(request: &Request<Body>, name: &str) -> Vec<String> {
    let decode = |text: &str| {
        percent_encoding::percent_decode_str(&text.replace('+', " "))
            .decode_utf8_lossy()
            .into_owned()
    };
    request
        .uri()
        .query()
        .unwrap_or_default()
        .split('&')
        .filter(|part| !part.is_empty())
        .filter_map(|part| {
            let (key, value) = part.split_once('=').unwrap_or((part, ""));
            (decode(key) == name).then(|| decode(value))
        })
        .collect()
}

/// `c.Request.Host`, which MSX uses instead of any forwarded host.
fn request_host(request: &Request<Body>) -> String {
    request
        .headers()
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
        .or_else(|| request.uri().authority().map(ToString::to_string))
        .unwrap_or_default()
}

fn scheme(state: &AppState, request: &Request<Body>) -> String {
    state
        .http
        .public_scheme(peer(request), request.headers(), request.uri())
}

/// gin's `DataFromReader` over an upstream response: status, length and
/// content type are copied, every other upstream header is dropped. A
/// transport error is a bare `500`.
fn relay(result: reqwest::Result<reqwest::Response>) -> Response<Body> {
    let Ok(upstream) = result else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let status = StatusCode::from_u16(upstream.status().as_u16())
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let length = upstream.headers().get(header::CONTENT_LENGTH).cloned();
    let content_type = upstream
        .headers()
        .get(header::CONTENT_TYPE)
        .cloned()
        .unwrap_or_else(|| HeaderValue::from_static(""));
    let mut response = Response::new(Body::from_stream(upstream.bytes_stream()));
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, content_type);
    if let Some(length) = length {
        headers.insert(header::CONTENT_LENGTH, length);
    }
    response
}

/// Go's `http.Redirect` answering a GET or HEAD request.
pub(crate) fn go_redirect(location: &str, status: StatusCode, method: &Method) -> Response<Body> {
    let mut escaped = String::with_capacity(location.len());
    for byte in location.bytes() {
        if byte.is_ascii() {
            escaped.push(char::from(byte));
        } else {
            escaped.push_str(&format!("%{byte:02x}"));
        }
    }
    let mut response = status.into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::LOCATION,
        HeaderValue::from_str(&escaped).unwrap_or_else(|_| HeaderValue::from_static("/")),
    );
    if matches!(*method, Method::GET | Method::HEAD) {
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/html; charset=utf-8"),
        );
    }
    if *method == Method::GET {
        let text = status.canonical_reason().unwrap_or_default();
        let body = format!("<a href=\"{}\">{text}</a>.\n\n", html_escape(location));
        headers.insert(header::CONTENT_LENGTH, body.len().into());
        *response.body_mut() = Body::from(body);
    }
    response
}

/// gin's trailing-slash redirect for `/msx`, the one MSX route whose
/// canonical form ends with a slash.
pub(crate) async fn landing_redirect(request: Request<Body>) -> Response<Body> {
    let location = match request.uri().query() {
        Some(query) => format!("/msx/?{query}"),
        None => "/msx/".to_owned(),
    };
    go_redirect(&location, StatusCode::MOVED_PERMANENTLY, request.method())
}

pub(crate) async fn landing(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response<Body>, Response<Body>> {
    if !management_authorized(&state, &headers) {
        return Err(unauthorized());
    }
    let msx = &state.integrations.msx;
    Ok(relay(msx.client.get(&msx.landing_url).send().await))
}

#[derive(Serialize)]
struct Launcher {
    image: String,
    #[serde(rename = "type")]
    kind: &'static str,
}

#[derive(Serialize)]
struct Start {
    launcher: Launcher,
    name: &'static str,
    parameter: String,
    version: String,
}

pub(crate) async fn start(
    State(state): State<AppState>,
    request: Request<Body>,
) -> Result<Response<Body>, Response<Body>> {
    if !management_authorized(&state, request.headers()) {
        return Err(unauthorized());
    }
    let image = format!(
        "{}://{}/logo.png",
        scheme(&state, &request),
        request_host(&request)
    );
    json_response(Start {
        launcher: Launcher {
            image,
            kind: "start",
        },
        name: "TorrServer",
        parameter: state.integrations.msx.parameter(),
        version: state.info.version.clone(),
    })
    .map_err(IntoResponse::into_response)
}

pub(crate) async fn set_start(
    State(state): State<AppState>,
    request: Request<Body>,
) -> Result<Response<Body>, Response<Body>> {
    if !management_authorized(&state, request.headers()) {
        return Err(unauthorized());
    }
    let body = body_bytes(request).await?;
    match go_decode::string(&body) {
        Ok(Some(parameter)) => state.integrations.msx.set_parameter(parameter),
        Ok(None) => {}
        Err(_) => return Err(ApiError::Status(StatusCode::BAD_REQUEST).into_response()),
    }
    Ok(StatusCode::OK.into_response())
}

/// `GET /msx/trn`: whether the hash names a saved torrent.
pub(crate) async fn saved(
    State(state): State<AppState>,
    request: Request<Body>,
) -> Result<Response<Body>, Response<Body>> {
    if !management_authorized(&state, request.headers()) {
        return Err(unauthorized());
    }
    let hash = first_query(&request, "hash");
    let mut found = false;
    if !hash.is_empty() {
        let TorrentReply::Magnets(saved) = state
            .core
            .torrents(TorrentCommand::Magnets)
            .await
            .map_err(|error| lifecycle(error).into_response())?
        else {
            return Err(ApiError::Status(StatusCode::INTERNAL_SERVER_ERROR).into_response());
        };
        found = saved.iter().any(|torrent| torrent.hash.to_string() == hash);
    }
    json_response(found).map_err(IntoResponse::into_response)
}

#[derive(Serialize)]
struct Envelope {
    response: Reply,
}

#[derive(Serialize)]
struct Reply {
    status: u16,
    text: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
}

impl Reply {
    fn new(status: StatusCode) -> Self {
        Self {
            status: status.as_u16(),
            text: status.canonical_reason().unwrap_or_default(),
            message: None,
            data: None,
        }
    }
}

/// `trn`: a status label and colour for a torrent that is loaded and not
/// closed, empty otherwise. An invalid hash panics in the reference
/// (`metainfo.NewHashFromHex`), which gin turns into a bare `500`.
async fn label(state: &AppState, hash: &str) -> Result<(String, String), Response<Body>> {
    let valid = hash.len() == 40 && hash.bytes().all(|byte| byte.is_ascii_hexdigit());
    let hash = valid
        .then(|| hash.to_ascii_lowercase().parse::<InfoHash>().ok())
        .flatten()
        .ok_or_else(|| StatusCode::INTERNAL_SERVER_ERROR.into_response())?;
    let reply = state
        .core
        .torrents(TorrentCommand::Get(hash))
        .await
        .map_err(|error| lifecycle(error).into_response())?;
    let TorrentReply::Torrent(Some(torrent)) = reply else {
        return Ok(Default::default());
    };
    if torrent.stat >= 5 {
        return Ok(Default::default());
    }
    let colour = match torrent.stat {
        4 => "msx-red",
        3 => "msx-green",
        _ => "msx-yellow",
    };
    let text = format!(
        "{{ico:north}} {} / {} {{ico:south}} {}",
        torrent.active_peers.unwrap_or_default(),
        torrent.total_peers.unwrap_or_default(),
        torrent.connected_seeders.unwrap_or_default()
    );
    Ok((text, colour.to_owned()))
}

/// `POST /msx/trn`: the player label for `?hash=`, or the MSX action data
/// for a JSON body `{"data": "…:<hash>"}`.
pub(crate) async fn status(
    State(state): State<AppState>,
    request: Request<Body>,
) -> Result<Response<Body>, Response<Body>> {
    if !management_authorized(&state, request.headers()) {
        return Err(unauthorized());
    }
    let hash = first_query(&request, "hash");
    if !hash.is_empty() {
        let (text, colour) = label(&state, &hash).await?;
        let colour = if colour.is_empty() {
            colour
        } else {
            format!("{{col:{colour}}}")
        };
        let mut data = Map::new();
        data.insert(
            "action".into(),
            format!("player:label:position:{{VALUE}}{{tb}}{{tb}}{colour}{text}").into(),
        );
        let mut reply = Reply::new(StatusCode::OK);
        reply.data = Some(data.into());
        return json_response(Envelope { response: reply }).map_err(IntoResponse::into_response);
    }
    let over_action = format!(
        "execute:{}://{}{}",
        scheme(&state, &request),
        request_host(&request),
        request.uri().path()
    );
    let body = body_bytes(request).await?;
    let data = match go_decode::data_field(&body) {
        Ok(data) => data,
        Err(message) => {
            // BindJSON has already written the 400 status, so the JSON that
            // follows goes out without its content type and is sniffed.
            let mut reply = Reply::new(StatusCode::BAD_REQUEST);
            reply.message = Some(message);
            let mut response =
                json_response(Envelope { response: reply }).map_err(IntoResponse::into_response)?;
            *response.status_mut() = StatusCode::BAD_REQUEST;
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/plain; charset=utf-8"),
            );
            return Ok(response);
        }
    };
    if data.is_empty() {
        let mut reply = Reply::new(StatusCode::BAD_REQUEST);
        reply.message = Some("data is not set".into());
        return json_response(Envelope { response: reply }).map_err(IntoResponse::into_response);
    }
    let hash = data
        .rsplit_once(':')
        .map_or(data.as_str(), |(_, hash)| hash);
    let (text, colour) = label(&state, hash).await?;
    // Keys in the order Go writes a map: sorted.
    let mut inner = Map::new();
    if !colour.is_empty() {
        let mut over = Map::new();
        over.insert("action".into(), over_action.into());
        over.insert("data".into(), data.clone().into());
        let mut live = Map::new();
        live.insert("duration".into(), 3000.into());
        live.insert("over".into(), over.into());
        live.insert("type".into(), "airtime".into());
        inner.insert("live".into(), live.into());
    }
    inner.insert("stamp".into(), text.into());
    inner.insert("stampColor".into(), colour.into());
    let mut outer = Map::new();
    outer.insert("action".into(), data.into());
    outer.insert("data".into(), inner.into());
    let mut reply = Reply::new(StatusCode::OK);
    reply.data = Some(outer.into());
    json_response(Envelope { response: reply }).map_err(IntoResponse::into_response)
}

/// `/msx/proxy?url=…&header=Name:value`: the request, with its method and
/// body, sent to `url` through the shared outbound client.
pub(crate) async fn proxy(
    State(state): State<AppState>,
    request: Request<Body>,
) -> Result<Response<Body>, Response<Body>> {
    if !management_authorized(&state, request.headers()) {
        return Err(unauthorized());
    }
    let url = first_query(&request, "url");
    if url.is_empty() {
        return Err(ApiError::Status(StatusCode::BAD_REQUEST).into_response());
    }
    let failed = || StatusCode::INTERNAL_SERVER_ERROR.into_response();
    let mut headers = reqwest::header::HeaderMap::new();
    for value in query_values(&request, "header") {
        let Some((name, value)) = value.split_once(':') else {
            continue;
        };
        // Go rejects the request when it sends an invalid header, and writes
        // values trimmed.
        let name = HeaderName::from_bytes(name.as_bytes()).map_err(|_| failed())?;
        let value = HeaderValue::from_str(value.trim_matches([' ', '\t'])).map_err(|_| failed())?;
        headers.append(name, value);
    }
    let method =
        reqwest::Method::from_bytes(request.method().as_str().as_bytes()).map_err(|_| failed())?;
    let url = reqwest::Url::parse(&url).map_err(|_| failed())?;
    let body = request.into_body();
    let mut outbound = state
        .integrations
        .msx
        .client
        .request(method, url)
        .headers(headers);
    // Go forwards the server's request body as a stream of unknown length.
    if body.size_hint().exact() != Some(0) {
        outbound = outbound.body(reqwest::Body::wrap_stream(body.into_data_stream()));
    }
    Ok(relay(outbound.send().await))
}

#[derive(serde::Deserialize)]
struct Suggestions {
    #[serde(default, alias = "D")]
    d: Vec<Suggestion>,
}

#[derive(serde::Deserialize)]
struct Suggestion {
    #[serde(default, alias = "I")]
    i: SuggestionImage,
}

#[derive(serde::Deserialize, Default)]
struct SuggestionImage {
    #[serde(default, rename = "imageUrl", alias = "ImageUrl", alias = "imageurl")]
    image_url: String,
}

/// `/msx/imdb/:id`: a redirect to the IMDb poster, or the suggestion JSON
/// itself when the id already ends with `.json`.
pub(crate) async fn imdb(
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
    request: Request<Body>,
) -> Result<Response<Body>, Response<Body>> {
    if !management_authorized(&state, request.headers()) {
        return Err(unauthorized());
    }
    let raw = id.ends_with(".json");
    let id = if raw { id } else { format!("{id}.json") };
    let msx = &state.integrations.msx;
    let result = msx.client.get(format!("{}{id}", msx.imdb_url)).send().await;
    let upstream = match result {
        Ok(upstream) if upstream.status() == reqwest::StatusCode::OK && !raw => upstream,
        other => return Ok(relay(other)),
    };
    let failed = || StatusCode::INTERNAL_SERVER_ERROR.into_response();
    let bytes = upstream.bytes().await.map_err(|_| failed())?;
    let suggestions: Suggestions = serde_json::from_slice(&bytes).map_err(|_| failed())?;
    match suggestions
        .d
        .first()
        .map(|first| first.i.image_url.as_str())
    {
        Some(url) if !url.is_empty() => Ok(go_redirect(
            url,
            StatusCode::MOVED_PERMANENTLY,
            request.method(),
        )),
        _ => Ok(StatusCode::NOT_FOUND.into_response()),
    }
}

/// `GET /files`: the target of the media link, `""` without one.
pub(crate) async fn media_link(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response<Body>, Response<Body>> {
    if !management_authorized(&state, &headers) {
        return Err(unauthorized());
    }
    let Some(media) = &state.integrations.msx.media else {
        return json_response("").map_err(IntoResponse::into_response);
    };
    match tokio::fs::read_link(media).await {
        Ok(target) => json_response(target.to_string_lossy()).map_err(IntoResponse::into_response),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            json_response("").map_err(IntoResponse::into_response)
        }
        // The reference passes the error's method value to c.JSON, which
        // cannot marshal it: a 500 with the JSON content type and no body.
        Err(_) => Ok((
            StatusCode::INTERNAL_SERVER_ERROR,
            [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        )
            .into_response()),
    }
}

/// `POST /files` with a JSON string: replaces the media link, or removes it
/// for `""`.
pub(crate) async fn set_media_link(
    State(state): State<AppState>,
    request: Request<Body>,
) -> Result<Response<Body>, Response<Body>> {
    if !management_authorized(&state, request.headers()) {
        return Err(unauthorized());
    }
    let status = |status: StatusCode| ApiError::Status(status).into_response();
    let body = body_bytes(request).await?;
    let target = go_decode::string(&body)
        .map_err(|_| status(StatusCode::BAD_REQUEST))?
        .unwrap_or_default();
    let Some(media) = &state.integrations.msx.media else {
        return Err(status(StatusCode::INTERNAL_SERVER_ERROR));
    };
    if let Err(error) = remove(media).await
        && error.kind() != io::ErrorKind::NotFound
    {
        return Err(status(StatusCode::INTERNAL_SERVER_ERROR));
    }
    if target.is_empty() {
        return Ok(StatusCode::OK.into_response());
    }
    match tokio::fs::metadata(&target).await {
        Ok(metadata) if metadata.is_dir() => {}
        _ => return Err(status(StatusCode::BAD_REQUEST)),
    }
    tokio::fs::symlink(&target, media)
        .await
        .map_err(|_| status(StatusCode::INTERNAL_SERVER_ERROR))?;
    Ok(StatusCode::OK.into_response())
}

/// Go's `os.Remove`: a file, a link or an empty directory.
async fn remove(path: &Path) -> io::Result<()> {
    match tokio::fs::remove_file(path).await {
        Err(error) if error.kind() != io::ErrorKind::NotFound => {
            tokio::fs::remove_dir(path).await.map_err(|_| error)
        }
        other => other,
    }
}

/// `GET`/`HEAD /files/*filepath`: the linked directory.
pub(crate) async fn files(
    State(state): State<AppState>,
    request: Request<Body>,
) -> Result<Response<Body>, Response<Body>> {
    if !management_authorized(&state, request.headers()) {
        return Err(unauthorized());
    }
    let Some(media) = &state.integrations.msx.media else {
        return Err(StatusCode::NOT_FOUND.into_response());
    };
    let raw = request.uri().path().strip_prefix("/files").unwrap_or("/");
    let path = percent_encoding::percent_decode_str(raw)
        .decode_utf8_lossy()
        .into_owned();
    Ok(file_server::serve(
        media,
        FileRequest {
            path: &path,
            query: request.uri().query(),
            method: request.method(),
            headers: request.headers(),
        },
    )
    .await)
}

#[cfg(test)]
mod tests {
    use std::{net::SocketAddr, sync::Arc};

    use axum::{Router, body::to_bytes, routing::get};
    use rustorr_lifecycle::ClientCore;
    use tower::ServiceExt;

    use super::*;
    use crate::{
        HttpConfig, Integrations, ServerInfo, app::tests::playback_core, router_with_services,
    };

    /// Stands in for the MSX front end, IMDb and any proxied site.
    async fn upstream() -> String {
        async fn echo(request: Request<Body>) -> Response<Body> {
            let method = request.method().to_string();
            let fixture = request
                .headers()
                .get("x-fixture")
                .map(|value| value.to_str().unwrap().to_owned())
                .unwrap_or_default();
            let body = to_bytes(request.into_body(), usize::MAX).await.unwrap();
            let text = format!("{method}|{fixture}|{}", String::from_utf8_lossy(&body));
            (
                [
                    (header::CONTENT_TYPE, "text/x-echo"),
                    (HeaderName::from_static("x-upstream"), "dropped"),
                ],
                text,
            )
                .into_response()
        }
        let routes = Router::new()
            .route(
                "/landing",
                get(|| async { ([(header::CONTENT_TYPE, "text/html")], "landing") }),
            )
            .route("/echo", axum::routing::any(echo))
            .route(
                "/redirect",
                axum::routing::any(|| async { (StatusCode::FOUND, [(header::LOCATION, "/echo")]) }),
            )
            .route(
                "/imdb/tt1.json",
                get(|| async { r#"{"d":[{"i":{"imageUrl":"http://img.invalid/p.jpg"}}]}"# }),
            )
            .route("/imdb/tt2.json", get(|| async { r#"{"d":[]}"# }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address: SocketAddr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
        format!("http://{address}")
    }

    async fn app(data_dir: Option<&Path>) -> (Router, String) {
        let base = upstream().await;
        let (core, _) = playback_core();
        let core: Arc<dyn ClientCore> = core;
        let integrations = Integrations {
            msx: Arc::new(Msx::with_endpoints(
                reqwest::Client::new(),
                data_dir,
                format!("{base}/landing"),
                format!("{base}/imdb/"),
            )),
            ..Integrations::default()
        };
        let router = router_with_services(
            ServerInfo {
                version: "MatriX.145".into(),
            },
            core,
            integrations,
            HttpConfig::default(),
        );
        (router, base)
    }

    async fn call(
        app: &Router,
        method: Method,
        uri: &str,
        body: &str,
    ) -> (StatusCode, HeaderMap, String) {
        let request = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::HOST, "msx.invalid:8090")
            .body(Body::from(body.to_owned()))
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, headers, String::from_utf8(body.to_vec()).unwrap())
    }

    const HASH: &str = "0101010101010101010101010101010101010101";

    #[tokio::test]
    async fn the_start_document_follows_the_posted_parameter() {
        let (app, _) = app(None).await;

        let (status, _, body) = call(&app, Method::GET, "/msx/start.json", "").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body,
            r#"{"launcher":{"image":"http://msx.invalid:8090/logo.png","type":"start"},"name":"TorrServer","parameter":"menu:request:interaction:{SERVER}@{PREFIX}tsmsx.yourok.ru/start.html","version":"MatriX.145"}"#
        );

        let (status, _, body) = call(&app, Method::POST, "/msx/start.json", r#""menu:x""#).await;
        assert_eq!((status, body.as_str()), (StatusCode::OK, ""));
        let (status, _, _) = call(&app, Method::POST, "/msx/start.json", "{").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (_, _, body) = call(&app, Method::GET, "/msx/start.json", "").await;
        assert!(body.contains(r#""parameter":"menu:x""#));
    }

    #[tokio::test]
    async fn trn_reports_saved_torrents_and_status_labels() {
        let (app, _) = app(None).await;

        let (_, _, body) = call(&app, Method::GET, &format!("/msx/trn?hash={HASH}"), "").await;
        assert_eq!(body, "true");
        let (_, _, body) = call(&app, Method::GET, "/msx/trn?hash=ff", "").await;
        assert_eq!(body, "false");

        let (status, _, body) =
            call(&app, Method::POST, &format!("/msx/trn?hash={HASH}"), "").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body,
            r#"{"response":{"status":200,"text":"OK","data":{"action":"player:label:position:{VALUE}{tb}{tb}{col:msx-green}{ico:north} 0 / 0 {ico:south} 0"}}}"#
        );

        let data = format!(r#"{{"data":"execute:http://x/y:{HASH}"}}"#);
        let (_, _, body) = call(&app, Method::POST, "/msx/trn", &data).await;
        assert_eq!(
            body,
            format!(
                r#"{{"response":{{"status":200,"text":"OK","data":{{"action":"execute:http://x/y:{HASH}","data":{{"live":{{"duration":3000,"over":{{"action":"execute:http://msx.invalid:8090/msx/trn","data":"execute:http://x/y:{HASH}"}},"type":"airtime"}},"stamp":"{{ico:north}} 0 / 0 {{ico:south}} 0","stampColor":"msx-green"}}}}}}}}"#
            )
        );

        let unknown = "0123456789abcdef0123456789abcdef01234567";
        let (_, _, body) = call(
            &app,
            Method::POST,
            "/msx/trn",
            &format!(r#"{{"Data":"a:{unknown}"}}"#),
        )
        .await;
        assert!(
            body.ends_with(r#""data":{"stamp":"","stampColor":""}}}}"#),
            "{body}"
        );

        let (status, _, body) = call(&app, Method::POST, "/msx/trn", "{}").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body,
            r#"{"response":{"status":400,"text":"Bad Request","message":"data is not set"}}"#
        );

        let (status, headers, body) = call(&app, Method::POST, "/msx/trn", "nope").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(headers[header::CONTENT_TYPE], "text/plain; charset=utf-8");
        assert_eq!(
            body,
            r#"{"response":{"status":400,"text":"Bad Request","message":"invalid character 'o' in literal null (expecting 'u')"}}"#
        );

        let (status, _, body) = call(&app, Method::POST, "/msx/trn?hash=xyz", "").await;
        assert_eq!(
            (status, body.as_str()),
            (StatusCode::INTERNAL_SERVER_ERROR, "")
        );
    }

    #[tokio::test]
    async fn the_proxy_relays_method_body_and_requested_headers_only() {
        let (app, base) = app(None).await;
        let echo = format!("{base}/echo");

        let uri = format!("/msx/proxy?url={echo}&header=X-Fixture:%20one&header=broken");
        let (status, headers, body) = call(&app, Method::GET, &uri, "").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "GET|one|");
        assert_eq!(headers[header::CONTENT_TYPE], "text/x-echo");
        assert_eq!(headers[header::CONTENT_LENGTH], "8");
        assert!(!headers.contains_key("x-upstream"));

        let (_, _, body) = call(
            &app,
            Method::PUT,
            &format!("/msx/proxy?url={echo}"),
            "payload",
        )
        .await;
        assert_eq!(body, "PUT||payload");

        let (_, _, body) = call(
            &app,
            Method::POST,
            &format!("/msx/proxy?url={base}/redirect"),
            "gone",
        )
        .await;
        assert_eq!(body, "GET||");

        let (status, _, _) = call(&app, Method::GET, "/msx/proxy", "").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _, body) = call(&app, Method::GET, "/msx/proxy?url=%3A%3A", "").await;
        assert_eq!(
            (status, body.as_str()),
            (StatusCode::INTERNAL_SERVER_ERROR, "")
        );
    }

    #[tokio::test]
    async fn landing_and_imdb_reach_their_upstreams() {
        let (app, _) = app(None).await;

        let (status, headers, body) = call(&app, Method::GET, "/msx/", "").await;
        assert_eq!((status, body.as_str()), (StatusCode::OK, "landing"));
        assert_eq!(headers[header::CONTENT_TYPE], "text/html");

        let (status, headers, body) = call(&app, Method::GET, "/msx", "").await;
        assert_eq!(status, StatusCode::MOVED_PERMANENTLY);
        assert_eq!(headers[header::LOCATION], "/msx/");
        assert_eq!(body, "<a href=\"/msx/\">Moved Permanently</a>.\n\n");

        let (status, headers, _) = call(&app, Method::GET, "/msx/imdb/tt1", "").await;
        assert_eq!(status, StatusCode::MOVED_PERMANENTLY);
        assert_eq!(headers[header::LOCATION], "http://img.invalid/p.jpg");
        let (status, _, body) = call(&app, Method::GET, "/msx/imdb/tt2", "").await;
        assert_eq!((status, body.as_str()), (StatusCode::NOT_FOUND, ""));
        let (status, _, body) = call(&app, Method::GET, "/msx/imdb/tt1.json", "").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("imageUrl"));
    }

    #[tokio::test]
    async fn files_links_serves_and_unlinks_a_media_directory() {
        let data = tempfile::tempdir().unwrap();
        let media = tempfile::tempdir().unwrap();
        std::fs::write(media.path().join("a.txt"), "a").unwrap();
        let (app, _) = app(Some(data.path())).await;
        let target = media.path().to_str().unwrap();

        let (_, _, body) = call(&app, Method::GET, "/files", "").await;
        assert_eq!(body, r#""""#);
        let (status, _, _) = call(&app, Method::GET, "/files/", "").await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let (status, _, body) = call(&app, Method::POST, "/files", &format!("{target:?}")).await;
        assert_eq!((status, body.as_str()), (StatusCode::OK, ""));
        let (_, _, body) = call(&app, Method::GET, "/files", "").await;
        assert_eq!(body, format!("{target:?}"));
        let (status, _, body) = call(&app, Method::GET, "/files/", "").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("<a href=\"a.txt\">a.txt</a>"));
        let (status, _, body) = call(&app, Method::HEAD, "/files/a.txt", "").await;
        assert_eq!((status, body.as_str()), (StatusCode::OK, ""));

        let file = format!("{:?}", media.path().join("a.txt").to_str().unwrap());
        let (status, _, _) = call(&app, Method::POST, "/files", &file).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _, _) = call(&app, Method::POST, "/files", "{").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let (status, _, _) = call(&app, Method::POST, "/files", r#""""#).await;
        assert_eq!(status, StatusCode::OK);
        let (_, _, body) = call(&app, Method::GET, "/files", "").await;
        assert_eq!(body, r#""""#);
    }

    #[test]
    fn go_redirect_writes_the_html_body_only_for_get() {
        let response = go_redirect("/msx/", StatusCode::MOVED_PERMANENTLY, &Method::GET);
        assert_eq!(response.headers()[header::LOCATION], "/msx/");
        assert_eq!(response.headers()[header::CONTENT_LENGTH], "40");

        let response = go_redirect("/msx/", StatusCode::MOVED_PERMANENTLY, &Method::HEAD);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/html; charset=utf-8"
        );
        assert!(!response.headers().contains_key(header::CONTENT_LENGTH));

        let response = go_redirect(
            "http://x/постер.jpg",
            StatusCode::MOVED_PERMANENTLY,
            &Method::GET,
        );
        assert_eq!(
            response.headers()[header::LOCATION],
            "http://x/%d0%bf%d0%be%d1%81%d1%82%d0%b5%d1%80.jpg"
        );
    }
}
