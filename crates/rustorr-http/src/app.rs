use std::{
    any::Any,
    collections::HashMap,
    future::Future,
    io,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use async_stream::try_stream;
use axum::{
    Router,
    body::{Body, Bytes, to_bytes},
    extract::{ConnectInfo, Multipart, Path, State},
    http::{
        HeaderMap, HeaderName, HeaderValue, Method, Request, Response, StatusCode, Uri, header,
    },
    middleware::{Next, from_fn, from_fn_with_state, map_response},
    response::IntoResponse,
    routing::{any, get, post},
};
use rustorr_lifecycle::{
    AddTorrent, CacheCommand, ClientCore, InfoHash, PlaybackRequest, Settings, SettingsCommand,
    TorrentCommand, TorrentCoordinator, TorrentReply, TorrentView, UpdateTorrent, ViewedCommand,
    WafCommand, WafLists, link_info_hash,
};
use rustorr_search::Search;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;
use tokio_util::io::ReaderStream;
use tower_http::{
    catch_panic::CatchPanicLayer,
    trace::{DefaultOnResponse, TraceLayer},
};
use tracing::{Level, error, info_span};

use crate::{
    ApiError,
    access::{HttpConfig, WafSnapshot},
    access_log,
    discovery::{Discovery, DiscoveryChange, NoDiscovery},
    error::go_json,
    ffprobe_api,
    gstreamer_api::{self, GstreamerSetup},
    listeners::Listeners,
    m3u,
    msx_api::{self, Msx},
    range::{self, ByteRange, RangeError},
    search_api, settings_api, web_api, web_ui,
    webdav::{self, WebDav},
};

#[derive(Debug, Clone)]
pub struct ServerInfo {
    pub version: String,
}

/// Integrations beside the client core that the HTTP surface exposes.
#[derive(Clone)]
pub struct Integrations {
    pub search: Arc<dyn Search>,
    pub msx: Arc<Msx>,
    pub discovery: Arc<dyn Discovery>,
    /// The GStreamer HLS module; without it `/gst` answers as a build
    /// without GStreamer.
    pub gstreamer: Option<GstreamerSetup>,
}

impl Default for Integrations {
    /// Nothing wired, as in router tests: no search, no media directory.
    fn default() -> Self {
        Self {
            search: Arc::new(search_api::NoSearch),
            msx: Arc::new(Msx::detached()),
            discovery: Arc::new(NoDiscovery),
            gstreamer: None,
        }
    }
}

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) info: ServerInfo,
    pub(crate) core: Arc<dyn ClientCore>,
    pub(crate) integrations: Integrations,
    pub(crate) http: HttpConfig,
    /// Present when `/dav` is enabled.
    pub(crate) webdav: Option<Arc<WebDav>>,
    /// Present when a GStreamer runtime was provided.
    pub(crate) gstreamer: Option<Arc<rustorr_gstreamer::service::Service>>,
    pub(crate) metrics: Arc<crate::metrics::HttpMetrics>,
}

pub fn router(info: ServerInfo) -> Router {
    with_basic_layers(
        Router::new()
            .route("/echo", get(move || echo(info.clone())))
            .fallback(not_found)
            .method_not_allowed_fallback(not_found),
    )
}

pub fn router_with_lifecycle(info: ServerInfo, torrents: Arc<TorrentCoordinator>) -> Router {
    let core: Arc<dyn ClientCore> = torrents;
    router_with_core(info, core, HttpConfig::default())
}

pub fn router_with_core(info: ServerInfo, core: Arc<dyn ClientCore>, http: HttpConfig) -> Router {
    router_with_services(info, core, Integrations::default(), http)
}

pub fn router_with_services(
    info: ServerInfo,
    core: Arc<dyn ClientCore>,
    integrations: Integrations,
    http: HttpConfig,
) -> Router {
    let webdav = http
        .webdav
        .then(|| Arc::new(WebDav::new(Arc::clone(&core))));
    let gstreamer = integrations
        .gstreamer
        .as_ref()
        .map(|setup| gstreamer_api::build_service(setup, Arc::clone(&core), http.port));
    let state = AppState {
        info,
        core,
        integrations,
        http,
        webdav,
        gstreamer,
        metrics: Arc::default(),
    };
    let routes = Router::new()
        .route("/echo", get(echo_state))
        .route("/metrics", get(crate::metrics::handler))
        .route("/torrents", post(torrents))
        .route("/torrent/upload", post(upload))
        .route("/settings", post(settings))
        .route("/viewed", post(viewed))
        .route("/cache", post(cache))
        .route("/waf", get(get_waf).post(set_waf))
        .route("/stream", get(stream_root).head(stream_root))
        // gin's `/stream/*fname` also matches `/stream/`, which GStreamer's
        // source URL uses; an axum wildcard needs a character after the slash.
        .route("/stream/", get(stream_root).head(stream_root))
        .route("/stream/{*fname}", get(stream_named).head(stream_named))
        .route("/play/{hash}/{id}", get(play).head(play))
        .route("/playlist", get(playlist_root))
        .route("/playlist/", get(playlist_root))
        .route("/playlist/{*fname}", get(playlist_named))
        .route("/playlistall/all.m3u", get(playlist_all))
        .route("/magnets", get(web_api::magnets))
        .route("/stat", get(web_api::stat))
        .route(
            "/storage/settings",
            get(settings_api::get_storage).post(settings_api::set_storage),
        )
        .route("/tmdb/settings", get(settings_api::tmdb))
        // gin's `/search/*query` also matches `/search` and `/search/`; an
        // axum wildcard needs at least one character after the slash.
        .route("/search", get(search_api::rutor))
        .route("/search/", get(search_api::rutor))
        .route("/search/{*query}", get(search_api::rutor))
        .route("/torznab/search", get(search_api::torznab))
        .route("/torznab/search/", get(search_api::torznab))
        .route("/torznab/search/{*query}", get(search_api::torznab))
        .route("/torznab/test", post(search_api::torznab_test))
        .route("/download/{size}", get(web_api::download))
        .route("/shutdown", get(web_api::shutdown))
        .route("/shutdown/{*reason}", get(web_api::shutdown))
        .route("/msx", get(msx_api::landing_redirect))
        .route("/msx/", get(msx_api::landing))
        .route(
            "/msx/start.json",
            get(msx_api::start).post(msx_api::set_start),
        )
        .route("/msx/trn", get(msx_api::saved).post(msx_api::status))
        .route("/msx/proxy", any(msx_api::proxy))
        .route("/msx/imdb/{id}", get(msx_api::imdb))
        .route(
            "/files",
            get(msx_api::media_link).post(msx_api::set_media_link),
        )
        .route("/files/", get(msx_api::files).head(msx_api::files))
        .route("/files/{*path}", get(msx_api::files).head(msx_api::files))
        .route(
            "/gst/settings",
            get(gstreamer_api::get_settings).post(gstreamer_api::set_settings),
        )
        .route("/ffp/status", get(ffprobe_api::status))
        .route("/ffp/{hash}/{id}", get(ffprobe_api::probe))
        .route("/dav", any(webdav::handle))
        .route("/dav/", any(webdav::handle))
        .route("/dav/{*path}", any(webdav::handle))
        .fallback(not_found)
        .method_not_allowed_fallback(not_found);
    let routes = web_ui::routes(routes);
    let routes = if state.gstreamer.is_some() {
        gstreamer_api::routes(routes)
    } else {
        routes
    };
    let routes = routes
        .with_state(state.clone())
        .layer(from_fn(head_only_where_routed))
        .layer(CatchPanicLayer::custom(panicked));
    Router::new()
        .fallback_service(routes)
        .layer(from_fn(cors))
        .layer(from_fn_with_state(state.clone(), waf))
        .layer(map_response(without_allow_on_404))
        // gin's first middleware: every request is logged, blocked or not.
        .layer(from_fn_with_state(state.clone(), access_log::middleware))
        .layer(from_fn_with_state(state, crate::metrics::middleware))
        .layer(
            TraceLayer::new_for_http()
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

pub async fn serve(
    listener: tokio::net::TcpListener,
    info: ServerInfo,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> io::Result<()> {
    axum::serve(
        listener,
        router(info).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await
}

pub async fn serve_with_lifecycle(
    listener: tokio::net::TcpListener,
    info: ServerInfo,
    torrents: Arc<TorrentCoordinator>,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> io::Result<()> {
    let core: Arc<dyn ClientCore> = torrents;
    serve_with_core(listener, info, core, HttpConfig::default(), shutdown).await
}

pub async fn serve_with_services(
    listener: Listeners,
    info: ServerInfo,
    core: Arc<dyn ClientCore>,
    integrations: Integrations,
    http: HttpConfig,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> io::Result<()> {
    listener
        .serve(
            router_with_services(info, core, integrations, http),
            shutdown,
        )
        .await
}

pub async fn serve_with_core(
    listener: tokio::net::TcpListener,
    info: ServerInfo,
    core: Arc<dyn ClientCore>,
    http: HttpConfig,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> io::Result<()> {
    axum::serve(
        listener,
        router_with_core(info, core, http).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await
}

fn with_basic_layers(routes: Router) -> Router {
    let routes = routes.layer(CatchPanicLayer::custom(panicked));
    Router::new()
        .fallback_service(routes)
        .layer(map_response(without_allow_on_404))
        .layer(
            TraceLayer::new_for_http()
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

async fn without_allow_on_404(mut response: Response<Body>) -> Response<Body> {
    if response.status() == StatusCode::NOT_FOUND {
        response.headers_mut().remove(header::ALLOW);
    }
    response
        .headers_mut()
        .insert(header::CONNECTION, HeaderValue::from_static("close"));
    response
}

async fn echo(info: ServerInfo) -> String {
    info.version
}

async fn echo_state(State(state): State<AppState>) -> String {
    state.info.version
}

pub(crate) fn peer(request: &Request<Body>) -> IpAddr {
    request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map_or(IpAddr::V4(Ipv4Addr::LOCALHOST), |peer| peer.0.ip())
}

async fn waf(State(state): State<AppState>, request: Request<Body>, next: Next) -> Response<Body> {
    let lists = match state.core.waf(WafCommand::Get).await {
        Ok(lists) => lists,
        Err(error) => {
            error!(%error, "cannot load WAF lists");
            return ApiError::Status(StatusCode::INTERNAL_SERVER_ERROR).into_response();
        }
    };
    if WafSnapshot::parse(lists).blocks(peer(&request), request.headers()) {
        return (StatusCode::FORBIDDEN, "Banned").into_response();
    }
    next.run(request).await
}

async fn cors(request: Request<Body>, next: Next) -> Response<Body> {
    if request.method() == Method::OPTIONS
        && request.headers().contains_key(header::ORIGIN)
        && request
            .headers()
            .contains_key(header::ACCESS_CONTROL_REQUEST_METHOD)
    {
        let mut response = StatusCode::NO_CONTENT.into_response();
        add_cors_headers(response.headers_mut(), request.headers());
        return response;
    }
    let request_headers = request.headers().clone();
    let mut response = next.run(request).await;
    if request_headers.contains_key(header::ORIGIN) {
        response.headers_mut().insert(
            header::ACCESS_CONTROL_ALLOW_ORIGIN,
            HeaderValue::from_static("*"),
        );
    }
    response
}

fn add_cors_headers(response: &mut HeaderMap, request: &HeaderMap) {
    response.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    response.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET,POST,PUT,PATCH,HEAD,OPTIONS,DELETE"),
    );
    response.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static(
            "Origin,Content-Length,Content-Type,X-Requested-With,Accept,Authorization,Mcp-Protocol-Version,Mcp-Session-Id,Last-Event-Id,Mcp-Method,Mcp-Name",
        ),
    );
    response.insert(
        header::ACCESS_CONTROL_MAX_AGE,
        HeaderValue::from_static("43200"),
    );
    if request
        .get("access-control-request-private-network")
        .is_some_and(|value| value == "true")
    {
        response.insert(
            HeaderName::from_static("access-control-allow-private-network"),
            HeaderValue::from_static("true"),
        );
    }
}

pub(crate) fn unauthorized() -> Response<Body> {
    Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .header(
            header::WWW_AUTHENTICATE,
            "Basic realm=Authorization Required",
        )
        .body(Body::empty())
        .expect("valid unauthorized response")
}

pub(crate) fn management_authorized(state: &AppState, headers: &HeaderMap) -> bool {
    state.http.authorized(headers)
}

pub(crate) async fn json_body<T: serde::de::DeserializeOwned>(
    request: Request<Body>,
) -> Result<T, ApiError> {
    let bytes = to_bytes(request.into_body(), 4 << 20)
        .await
        .map_err(|error| json_bad_request(error.to_string()))?;
    serde_json::from_slice(&bytes).map_err(|error| {
        let message = if error.is_eof() {
            "unexpected EOF".into()
        } else {
            error.to_string()
        };
        json_bad_request(message)
    })
}

/// MatriX.145's answer to a write refused in read-only DB mode.
pub(crate) fn read_only_refused() -> Response<Body> {
    ApiError::Json {
        status: StatusCode::FORBIDDEN,
        message: "Read-only mode".into(),
    }
    .into_response()
}

pub(crate) fn json_bad_request(message: impl Into<String>) -> ApiError {
    ApiError::Json {
        status: StatusCode::BAD_REQUEST,
        message: message.into(),
    }
}

pub(crate) fn lifecycle(error: impl std::fmt::Display) -> ApiError {
    json_bad_request(error.to_string())
}

pub(crate) fn json_response(value: impl Serialize) -> Result<Response<Body>, ApiError> {
    let bytes = go_json(&value).map_err(|_| ApiError::Status(StatusCode::INTERNAL_SERVER_ERROR))?;
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json; charset=utf-8")
        .header(header::CONTENT_LENGTH, bytes.len())
        .body(Body::from(bytes))
        .map_err(|_| ApiError::Status(StatusCode::INTERNAL_SERVER_ERROR))
}

#[derive(Debug, Deserialize)]
struct TorrentAction {
    action: String,
    #[serde(default)]
    link: String,
    hash: Option<String>,
    #[serde(default)]
    title: String,
    #[serde(default)]
    poster: String,
    #[serde(default)]
    category: String,
    #[serde(default)]
    data: String,
    #[serde(default)]
    save_to_db: bool,
}

async fn torrents(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: Request<Body>,
) -> Result<Response<Body>, Response<Body>> {
    if !management_authorized(&state, &headers) {
        return Err(unauthorized());
    }
    let action: TorrentAction = json_body(request)
        .await
        .map_err(IntoResponse::into_response)?;
    let command = match action.action.as_str() {
        "add" => {
            if action.link.is_empty() {
                return Err(json_bad_request("link is empty").into_response());
            }
            TorrentCommand::Add(AddTorrent {
                link: action.link,
                title: action.title,
                poster: action.poster,
                category: action.category,
                data: action.data,
                save_to_db: action.save_to_db,
            })
        }
        "get" => {
            TorrentCommand::Get(required_hash(action.hash).map_err(IntoResponse::into_response)?)
        }
        "set" => TorrentCommand::Set(UpdateTorrent {
            hash: required_hash(action.hash).map_err(IntoResponse::into_response)?,
            title: action.title,
            poster: action.poster,
            category: action.category,
            data: action.data,
        }),
        "rem" => {
            TorrentCommand::Remove(required_hash(action.hash).map_err(IntoResponse::into_response)?)
        }
        "list" => TorrentCommand::List,
        "drop" => {
            TorrentCommand::Drop(required_hash(action.hash).map_err(IntoResponse::into_response)?)
        }
        "wipe" => TorrentCommand::Wipe,
        other => {
            return Err(json_bad_request(format!("unknown action: \"{other}\"")).into_response());
        }
    };
    let changes_catalog = matches!(
        command,
        TorrentCommand::Add(_) | TorrentCommand::Remove(_) | TorrentCommand::Wipe
    );
    // The reference drops the torrent's HLS task with it.
    let removed_tasks: Vec<String> = match &command {
        TorrentCommand::Remove(hash) | TorrentCommand::Drop(hash) => vec![hash.to_string()],
        TorrentCommand::Wipe if state.gstreamer.is_some() => {
            match state.core.torrents(TorrentCommand::List).await {
                Ok(TorrentReply::List(torrents)) => torrents
                    .iter()
                    .filter_map(|torrent| torrent.hash.clone())
                    .collect(),
                _ => Vec::new(),
            }
        }
        _ => Vec::new(),
    };
    let reply = state
        .core
        .torrents(command)
        .await
        .map_err(|error| lifecycle(error).into_response())?;
    for hash in &removed_tasks {
        gstreamer_api::remove_task(&state, hash);
    }
    // MatriX.145 restarts its DLNA server after these, when it is enabled.
    if changes_catalog
        && state
            .core
            .settings(SettingsCommand::Get)
            .await
            .is_ok_and(|settings| settings.enable_dlna)
    {
        state.integrations.discovery.catalog_changed().await;
    }
    match reply {
        TorrentReply::Torrent(Some(torrent)) => {
            json_response(torrent).map_err(IntoResponse::into_response)
        }
        TorrentReply::Torrent(None) => Err(StatusCode::NOT_FOUND.into_response()),
        TorrentReply::List(torrents) => {
            json_response(torrents).map_err(IntoResponse::into_response)
        }
        // `/torrents` never asks for the magnet list.
        TorrentReply::Empty | TorrentReply::Magnets(_) => Ok(StatusCode::OK.into_response()),
    }
}

fn required_hash(value: Option<String>) -> Result<InfoHash, ApiError> {
    let value = value
        .filter(|value| !value.is_empty())
        .ok_or_else(|| json_bad_request("hash is empty"))?;
    value
        .parse()
        .map_err(|_| json_bad_request("invalid info hash"))
}

async fn upload(
    State(state): State<AppState>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Result<Response<Body>, Response<Body>> {
    if !management_authorized(&state, &headers) {
        return Err(unauthorized());
    }
    let mut options = AddTorrent::default();
    let mut files = Vec::new();
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|error| json_bad_request(error.to_string()).into_response())?
    {
        let name = field.name().unwrap_or_default().to_owned();
        if field.file_name().is_some() {
            files.push(
                field
                    .bytes()
                    .await
                    .map_err(|error| json_bad_request(error.to_string()).into_response())?
                    .to_vec(),
            );
            continue;
        }
        let value = field
            .text()
            .await
            .map_err(|error| json_bad_request(error.to_string()).into_response())?;
        match name.as_str() {
            "save" => options.save_to_db = true,
            "title" => options.title = value,
            "poster" => options.poster = value,
            "category" => options.category = value,
            "data" => options.data = value,
            _ => {}
        }
    }
    let mut added = Vec::new();
    for bytes in files {
        if let TorrentReply::Torrent(Some(torrent)) = state
            .core
            .torrents(TorrentCommand::AddMetainfo {
                bytes,
                request: options.clone(),
            })
            .await
            .map_err(|error| lifecycle(error).into_response())?
        {
            added.push(torrent);
        }
    }
    if added.len() == 1 {
        json_response(added.remove(0)).map_err(IntoResponse::into_response)
    } else {
        json_response(added).map_err(IntoResponse::into_response)
    }
}

#[derive(Debug, Deserialize)]
struct SettingsAction {
    action: String,
    sets: Option<Settings>,
}

async fn settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: Request<Body>,
) -> Result<Response<Body>, Response<Body>> {
    if !management_authorized(&state, &headers) {
        return Err(unauthorized());
    }
    let request: SettingsAction = json_body(request)
        .await
        .map_err(IntoResponse::into_response)?;
    let requested = request.sets.clone();
    let command = match request.action.as_str() {
        "get" => SettingsCommand::Get,
        "set" => SettingsCommand::Set(Box::new(
            request
                .sets
                .ok_or_else(|| json_bad_request("sets is empty").into_response())?,
        )),
        "def" => SettingsCommand::Defaults,
        _ => return Err(ApiError::Status(StatusCode::BAD_REQUEST).into_response()),
    };
    let settings = state
        .core
        .settings(command)
        .await
        .map_err(|error| lifecycle(error).into_response())?;
    // MatriX.145 restarts DLNA and Bonjour, then Rutor search, on every
    // change.
    let change = match (request.action.as_str(), requested) {
        ("set", Some(requested)) => Some(DiscoveryChange::Set {
            dlna: requested.enable_dlna,
            bonjour: requested.enable_bonjour,
            settings: Box::new(settings.clone()),
        }),
        ("def", _) => Some(DiscoveryChange::Defaults),
        _ => None,
    };
    if let Some(change) = change {
        state.integrations.discovery.settings_changed(change).await;
    }
    if request.action != "get" {
        state
            .integrations
            .search
            .set_rutor_enabled(settings.enable_rutor_search)
            .await;
    }
    if request.action == "get" {
        json_response(settings).map_err(IntoResponse::into_response)
    } else {
        Ok(StatusCode::OK.into_response())
    }
}

#[derive(Debug, Deserialize)]
struct ViewedAction {
    action: String,
    #[serde(default)]
    hash: String,
    #[serde(default)]
    file_index: i32,
    #[serde(default)]
    timecode: f64,
}

#[derive(Serialize)]
struct ViewedResponse {
    hash: String,
    file_index: u32,
    #[serde(serialize_with = "serialize_timecode")]
    timecode: f64,
}

fn serialize_timecode<S>(value: &f64, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    if value.fract() == 0.0 && *value >= i64::MIN as f64 && *value <= i64::MAX as f64 {
        serializer.serialize_i64(*value as i64)
    } else {
        serializer.serialize_f64(*value)
    }
}

async fn viewed(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: Request<Body>,
) -> Result<Response<Body>, Response<Body>> {
    if !management_authorized(&state, &headers) {
        return Err(unauthorized());
    }
    let request: ViewedAction = json_body(request)
        .await
        .map_err(IntoResponse::into_response)?;
    let hash = (!request.hash.is_empty())
        .then(|| request.hash.parse::<InfoHash>())
        .transpose()
        .map_err(|error| json_bad_request(error.to_string()).into_response())?;
    let command = match request.action.as_str() {
        "set" => ViewedCommand::Set {
            hash: hash.ok_or_else(|| json_bad_request("hash is required").into_response())?,
            index: u32::try_from(request.file_index)
                .map_err(|_| json_bad_request("file index is invalid").into_response())?,
            timecode: request.timecode,
        },
        "rem" => ViewedCommand::Remove {
            hash: hash.ok_or_else(|| json_bad_request("hash is required").into_response())?,
            index: (request.file_index != -1)
                .then(|| u32::try_from(request.file_index))
                .transpose()
                .map_err(|_| json_bad_request("file index is invalid").into_response())?,
        },
        "list" => ViewedCommand::List { hash },
        _ => return Ok(StatusCode::OK.into_response()),
    };
    let viewed = state
        .core
        .viewed(command)
        .await
        .map_err(|error| lifecycle(error).into_response())?;
    if request.action == "list" {
        json_response(
            viewed
                .into_iter()
                .map(|entry| ViewedResponse {
                    hash: entry.hash.to_string(),
                    file_index: entry.index,
                    timecode: entry.timecode,
                })
                .collect::<Vec<_>>(),
        )
        .map_err(IntoResponse::into_response)
    } else {
        Ok(StatusCode::OK.into_response())
    }
}

#[derive(Debug, Deserialize)]
struct CacheAction {
    action: String,
    #[serde(default)]
    hash: String,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct CacheResponse {
    hash: String,
    capacity: u64,
    filled: u64,
    pieces_length: u64,
    pieces_count: u32,
    torrent: TorrentView,
    pieces: HashMap<u32, CachePiece>,
    readers: Vec<CacheReader>,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct CachePiece {
    id: u32,
    length: u64,
    size: u64,
    completed: bool,
    priority: i32,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct CacheReader {
    start: u64,
    end: u64,
    reader: u32,
}

async fn cache(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: Request<Body>,
) -> Result<Response<Body>, Response<Body>> {
    if !management_authorized(&state, &headers) {
        return Err(unauthorized());
    }
    let request: CacheAction = json_body(request)
        .await
        .map_err(IntoResponse::into_response)?;
    if request.action != "get" || request.hash.is_empty() {
        return Err(ApiError::Status(StatusCode::BAD_REQUEST).into_response());
    }
    let hash: InfoHash = request
        .hash
        .parse()
        .map_err(|_| json_bad_request("invalid info hash").into_response())?;
    let view = cache_view(state.core.as_ref(), hash).await?;
    json_response(view).map_err(IntoResponse::into_response)
}

/// The cache state `/cache` and `/gst/:hash/heartbeat` report: `404` when the
/// torrent is not loaded.
pub(crate) async fn cache_view(
    core: &dyn ClientCore,
    hash: InfoHash,
) -> Result<impl Serialize + use<>, Response<Body>> {
    let cache = core
        .cache(CacheCommand::Get(hash))
        .await
        .map_err(|_| StatusCode::NOT_FOUND.into_response())?;
    let snapshot = cache
        .snapshots
        .first()
        .ok_or_else(|| StatusCode::NOT_FOUND.into_response())?;
    let torrent = match core
        .torrents(TorrentCommand::Get(hash))
        .await
        .map_err(|error| lifecycle(error).into_response())?
    {
        TorrentReply::Torrent(Some(torrent)) => *torrent,
        _ => return Err(StatusCode::NOT_FOUND.into_response()),
    };
    let pieces: HashMap<_, _> = if snapshot.demanded_bytes != 0 {
        snapshot
            .demanded_pieces
            .iter()
            .map(|piece| {
                (
                    piece.piece.get(),
                    CachePiece {
                        id: piece.piece.get(),
                        length: snapshot.piece_length,
                        size: piece.size,
                        completed: piece.completed,
                        priority: 0,
                    },
                )
            })
            .collect()
    } else {
        snapshot
            .completed_pieces
            .iter()
            .map(|piece| {
                (
                    piece.get(),
                    CachePiece {
                        id: piece.get(),
                        length: snapshot.piece_length,
                        size: snapshot.piece_length,
                        completed: true,
                        priority: 0,
                    },
                )
            })
            .collect()
    };
    Ok(CacheResponse {
        hash: hash.to_string(),
        capacity: cache.capacity,
        filled: if snapshot.demanded_bytes == 0 {
            snapshot.stored_bytes
        } else {
            snapshot.demanded_bytes
        },
        pieces_length: snapshot.piece_length,
        pieces_count: snapshot.piece_count,
        torrent,
        pieces,
        readers: snapshot
            .active_readers
            .iter()
            .enumerate()
            .map(|(index, range)| CacheReader {
                start: range.start / snapshot.piece_length,
                end: range.end.saturating_add(1).div_ceil(snapshot.piece_length),
                reader: u32::try_from(range.start / snapshot.piece_length)
                    .unwrap_or_else(|_| u32::try_from(index).unwrap_or(u32::MAX)),
            })
            .collect(),
    })
}

#[derive(Debug, Deserialize)]
struct WafUpdate {
    whitelist: String,
    blacklist: String,
    referers: String,
}

#[derive(Serialize)]
struct WafResponse {
    whitelist: String,
    blacklist: String,
    referers: String,
    ip_enabled: bool,
    referer_enabled: bool,
    read_only: bool,
    warnings: Vec<crate::access::WafWarning>,
}

/// Go's `http.Error`: plain text with a trailing newline and `nosniff`.
pub(crate) fn go_http_error(status: StatusCode, message: &str) -> Response<Body> {
    let mut response = (
        status,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        format!("{message}\n"),
    )
        .into_response();
    response.headers_mut().insert(
        HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    response
}

fn waf_response(lists: WafLists, read_only: bool) -> WafResponse {
    let snapshot = WafSnapshot::parse(lists.clone());
    WafResponse {
        whitelist: lists.whitelist,
        blacklist: lists.blacklist,
        referers: lists.referers,
        ip_enabled: snapshot.ip_enabled(),
        referer_enabled: snapshot.referer_enabled(),
        read_only,
        warnings: snapshot.warnings,
    }
}

async fn get_waf(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response<Body>, Response<Body>> {
    if !management_authorized(&state, &headers) {
        return Err(unauthorized());
    }
    let lists = state
        .core
        .waf(WafCommand::Get)
        .await
        .map_err(|error| lifecycle(error).into_response())?;
    json_response(waf_response(lists, state.http.read_only)).map_err(IntoResponse::into_response)
}

async fn set_waf(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: Request<Body>,
) -> Result<Response<Body>, Response<Body>> {
    if !management_authorized(&state, &headers) {
        return Err(unauthorized());
    }
    if state.http.read_only {
        return Err(read_only_refused());
    }
    let update: WafUpdate = json_body(request)
        .await
        .map_err(IntoResponse::into_response)?;
    let lists = state
        .core
        .waf(WafCommand::Set(WafLists {
            whitelist: update.whitelist,
            blacklist: update.blacklist,
            referers: update.referers,
        }))
        .await
        .map_err(|error| lifecycle(error).into_response())?;
    json_response(waf_response(lists, false)).map_err(IntoResponse::into_response)
}

async fn stream_root(
    State(state): State<AppState>,
    request: Request<Body>,
) -> Result<Response<Body>, Response<Body>> {
    let peer = peer(&request);
    stream_impl(state, peer, None, request).await
}

async fn stream_named(
    State(state): State<AppState>,
    Path(fname): Path<String>,
    request: Request<Body>,
) -> Result<Response<Body>, Response<Body>> {
    let peer = peer(&request);
    stream_impl(state, peer, Some(fname), request).await
}

pub(crate) fn query(uri: &Uri) -> HashMap<String, String> {
    uri.query()
        .unwrap_or_default()
        .split('&')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let (key, value) = part.split_once('=').unwrap_or((part, ""));
            (percent_decode(key), percent_decode(value))
        })
        .collect()
}

fn percent_decode(value: &str) -> String {
    percent_encoding::percent_decode_str(&value.replace('+', " "))
        .decode_utf8_lossy()
        .into_owned()
}

async fn stream_impl(
    state: AppState,
    peer: IpAddr,
    fname: Option<String>,
    request: Request<Body>,
) -> Result<Response<Body>, Response<Body>> {
    let query = query(request.uri());
    let link = query.get("link").cloned().unwrap_or_default();
    if link.is_empty() {
        return Err(json_bad_request("link should not be empty").into_response());
    }
    let play = query.contains_key("play");
    let playlist = query.contains_key("m3u");
    if state.http.auth_enabled()
        && !state.http.authorized(request.headers())
        && (!(play || playlist) || !shareable_link_exists(&state, &link).await)
    {
        return Err(unauthorized());
    }
    let torrent = match state
        .core
        .torrents(TorrentCommand::Add(AddTorrent {
            link,
            title: query.get("title").cloned().unwrap_or_default(),
            poster: query.get("poster").cloned().unwrap_or_default(),
            category: query.get("category").cloned().unwrap_or_default(),
            data: String::new(),
            save_to_db: query.contains_key("save"),
        }))
        .await
        .map_err(|error| lifecycle(error).into_response())?
    {
        TorrentReply::Torrent(Some(torrent)) => *torrent,
        _ => return Err(StatusCode::INTERNAL_SERVER_ERROR.into_response()),
    };
    if query.contains_key("preload") {
        let index = query
            .get("index")
            .and_then(|index| index.parse().ok())
            .unwrap_or(1);
        let hash = torrent
            .hash()
            .ok_or_else(|| StatusCode::INTERNAL_SERVER_ERROR.into_response())?;
        let playback = state
            .core
            .clone()
            .playback(PlaybackRequest {
                hash,
                index,
                offset: 0,
                end: Some(0),
                prefetch_offset: Some(0),
            })
            .await
            .map_err(|error| lifecycle(error).into_response())?;
        drop(playback);
    }
    if query.contains_key("stat") {
        // `tor.Status()` after the add: an already running torrent reports
        // its current state, not the add's snapshot.
        let current = match torrent.hash() {
            Some(hash) => match state.core.torrents(TorrentCommand::Get(hash)).await {
                Ok(TorrentReply::Torrent(Some(current))) => *current,
                _ => torrent,
            },
            None => torrent,
        };
        return json_response(current).map_err(IntoResponse::into_response);
    }
    if playlist {
        let base = state
            .http
            .public_base(peer, request.headers(), request.uri());
        return individual_playlist(
            &state,
            &torrent,
            &base,
            fname.as_deref(),
            query.contains_key("fromlast"),
            query.get("index").and_then(|index| index.parse().ok()),
            request.headers(),
        )
        .await
        .map_err(IntoResponse::into_response);
    }
    if play {
        let index = query
            .get("index")
            .and_then(|index| index.parse().ok())
            .unwrap_or(1);
        return raw_playback(
            state.core,
            state.http.max_stream_size,
            torrent,
            index,
            request,
        )
        .await;
    }
    Ok(StatusCode::OK.into_response())
}

async fn shareable_link_exists(state: &AppState, link: &str) -> bool {
    let Some(hash) = link_info_hash(link) else {
        return false;
    };
    matches!(
        state.core.torrents(TorrentCommand::Get(hash)).await,
        Ok(TorrentReply::Torrent(Some(_)))
    )
}

async fn play(
    State(state): State<AppState>,
    Path((hash, id)): Path<(String, String)>,
    request: Request<Body>,
) -> Result<Response<Body>, Response<Body>> {
    let hash: InfoHash = hash
        .parse()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())?;
    match state.core.torrents(TorrentCommand::Get(hash)).await {
        Ok(TorrentReply::Torrent(Some(_))) => {}
        _ if state.http.auth_enabled() && !state.http.authorized(request.headers()) => {
            return Err(unauthorized());
        }
        _ => return Err(StatusCode::INTERNAL_SERVER_ERROR.into_response()),
    }
    let torrent = match state
        .core
        .torrents(TorrentCommand::Add(AddTorrent {
            link: hash.to_string(),
            ..AddTorrent::default()
        }))
        .await
    {
        Ok(TorrentReply::Torrent(Some(torrent))) => *torrent,
        _ => return Err(StatusCode::INTERNAL_SERVER_ERROR.into_response()),
    };
    // A single-file torrent plays its file whatever the index says; otherwise
    // the index must be a number (`strconv.Atoi`).
    let index = if torrent.file_stats.len() == 1 {
        torrent.file_stats[0].id
    } else {
        match id.parse::<i64>() {
            Ok(-1) | Err(_) => {
                return Err(ApiError::Status(StatusCode::BAD_REQUEST).into_response());
            }
            Ok(index) => u32::try_from(index).unwrap_or(u32::MAX),
        }
    };
    raw_playback(
        state.core,
        state.http.max_stream_size,
        torrent,
        index,
        request,
    )
    .await
}

async fn raw_playback(
    core: Arc<dyn ClientCore>,
    max_stream_size: Option<u64>,
    torrent: TorrentView,
    index: u32,
    request: Request<Body>,
) -> Result<Response<Body>, Response<Body>> {
    let hash = torrent
        .hash()
        .ok_or_else(|| StatusCode::INTERNAL_SERVER_ERROR.into_response())?;
    let file = torrent
        .file_stats
        .iter()
        .find(|file| file.id == index)
        .cloned()
        .ok_or_else(|| {
            json_bad_request(format!("file with id {index} not found")).into_response()
        })?;
    if let Some(limit) = max_stream_size.filter(|limit| file.length > *limit) {
        return Err(go_http_error(
            StatusCode::FORBIDDEN,
            &format!("file size exceeded max allowed {limit} bytes"),
        ));
    }
    let etag = file_etag(hash, &file.path);
    // Go's type table with TorrServer's extensions, as ServeContent uses it.
    let mime = crate::media_type::by_extension(&file.path)
        .unwrap_or_else(|| "application/octet-stream".into());
    if precondition_failed(request.headers(), &etag, torrent.timestamp) {
        let mut response = StatusCode::PRECONDITION_FAILED.into_response();
        playback_headers(
            response.headers_mut(),
            &mime,
            &etag,
            torrent.timestamp,
            &request,
        );
        response.headers_mut().remove(header::CONTENT_TYPE);
        return Ok(response);
    }
    if not_modified(request.headers(), &etag, torrent.timestamp) {
        let mut response = StatusCode::NOT_MODIFIED.into_response();
        playback_headers(
            response.headers_mut(),
            &mime,
            &etag,
            torrent.timestamp,
            &request,
        );
        response.headers_mut().remove(header::CONTENT_TYPE);
        response.headers_mut().remove(header::ACCEPT_RANGES);
        response.headers_mut().remove(header::LAST_MODIFIED);
        return Ok(response);
    }
    touch_viewed(&core, hash, index).await?;
    let ranges = if request
        .headers()
        .get(header::IF_RANGE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| !if_range_matches(value, &etag, torrent.timestamp))
    {
        None
    } else {
        request
            .headers()
            .get(header::RANGE)
            .and_then(|value| value.to_str().ok())
    };
    let ranges = match ranges.map(|value| range::parse(value, file.length)) {
        None => None,
        Some(Ok(ranges)) => Some(ranges),
        Some(Err(RangeError::Invalid | RangeError::Unsatisfiable)) => {
            let mut response = (
                StatusCode::RANGE_NOT_SATISFIABLE,
                "invalid range: failed to overlap\n",
            )
                .into_response();
            response.headers_mut().insert(
                header::CONTENT_RANGE,
                HeaderValue::from_str(&format!("bytes */{}", file.length)).unwrap(),
            );
            playback_headers(
                response.headers_mut(),
                &mime,
                &etag,
                torrent.timestamp,
                &request,
            );
            response.headers_mut().remove(header::ETAG);
            response.headers_mut().remove(header::LAST_MODIFIED);
            response.headers_mut().insert(
                HeaderName::from_static("x-content-type-options"),
                HeaderValue::from_static("nosniff"),
            );
            return Ok(response);
        }
    };
    let method = request.method().clone();
    let mut response = match ranges {
        None => {
            let playback = core
                .clone()
                .playback(PlaybackRequest {
                    hash,
                    index,
                    offset: 0,
                    end: Some(file.length.saturating_sub(1)),
                    prefetch_offset: None,
                })
                .await
                .map_err(|error| lifecycle(error).into_response())?;
            let body = if method == Method::HEAD {
                Body::empty()
            } else {
                Body::from_stream(ReaderStream::new(playback.reader.take(file.length)))
            };
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_LENGTH, file.length)
                .body(body)
                .expect("valid full response")
        }
        Some(ranges) if ranges.len() == 1 => {
            let range = ranges[0];
            let playback = core
                .clone()
                .playback(PlaybackRequest {
                    hash,
                    index,
                    offset: range.start,
                    end: Some(range.end),
                    prefetch_offset: None,
                })
                .await
                .map_err(|error| lifecycle(error).into_response())?;
            let body = if method == Method::HEAD {
                Body::empty()
            } else {
                Body::from_stream(ReaderStream::new(playback.reader.take(range.len())))
            };
            Response::builder()
                .status(StatusCode::PARTIAL_CONTENT)
                .header(header::CONTENT_LENGTH, range.len())
                .header(
                    header::CONTENT_RANGE,
                    format!("bytes {}-{}/{}", range.start, range.end, file.length),
                )
                .body(body)
                .expect("valid range response")
        }
        Some(ranges) => {
            multipart_response(
                core,
                hash,
                index,
                file.length,
                &mime,
                ranges,
                method == Method::HEAD,
            )
            .await?
        }
    };
    playback_headers(
        response.headers_mut(),
        &mime,
        &etag,
        torrent.timestamp,
        &request,
    );
    Ok(response)
}

async fn touch_viewed(
    core: &Arc<dyn ClientCore>,
    hash: InfoHash,
    index: u32,
) -> Result<(), Response<Body>> {
    let existing = core
        .viewed(ViewedCommand::List { hash: Some(hash) })
        .await
        .map_err(|error| lifecycle(error).into_response())?;
    let timecode = existing
        .iter()
        .find(|entry| entry.index == index)
        .map_or(0.0, |entry| entry.timecode);
    core.viewed(ViewedCommand::Set {
        hash,
        index,
        timecode,
    })
    .await
    .map_err(|error| lifecycle(error).into_response())?;
    Ok(())
}

fn playback_headers(
    headers: &mut HeaderMap,
    mime: &str,
    etag: &str,
    timestamp: i64,
    request: &Request<Body>,
) {
    if !headers.contains_key(header::CONTENT_TYPE) {
        headers.insert(header::CONTENT_TYPE, HeaderValue::from_str(mime).unwrap());
    }
    headers.insert(header::CONNECTION, HeaderValue::from_static("close"));
    headers.insert(
        header::SERVER,
        HeaderValue::from_static("TorrServer (Portable SDK for UPnP devices)"),
    );
    headers.insert(header::ETAG, HeaderValue::from_str(etag).unwrap());
    headers.insert(
        header::LAST_MODIFIED,
        HeaderValue::from_str(&httpdate::fmt_http_date(system_time(timestamp))).unwrap(),
    );
    headers.insert(
        HeaderName::from_static("x-stream-timeout"),
        HeaderValue::from_static("30"),
    );
    headers.insert(
        HeaderName::from_static("transfermode.dlna.org"),
        HeaderValue::from_static("Streaming"),
    );
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    if request
        .headers()
        .contains_key("getcontentfeatures.dlna.org")
    {
        headers.insert(
            HeaderName::from_static("contentfeatures.dlna.org"),
            HeaderValue::from_static(
                "DLNA.ORG_OP=01;DLNA.ORG_CI=0;DLNA.ORG_FLAGS=01700000000000000000000000000000",
            ),
        );
    }
}

fn file_etag(hash: InfoHash, path: &str) -> String {
    let value = format!("{hash}/{path}");
    let encoded: String = value
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("\"{encoded}\"")
}

fn system_time(timestamp: i64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(u64::try_from(timestamp).unwrap_or_default())
}

fn not_modified(headers: &HeaderMap, etag: &str, timestamp: i64) -> bool {
    if let Some(value) = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
    {
        return value.split(',').any(|value| {
            let value = value.trim().strip_prefix("W/").unwrap_or(value.trim());
            value == etag || value == "*"
        });
    }
    headers
        .get(header::IF_MODIFIED_SINCE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| httpdate::parse_http_date(value).ok())
        .is_some_and(|since| system_time(timestamp) <= since)
}

fn precondition_failed(headers: &HeaderMap, etag: &str, timestamp: i64) -> bool {
    if let Some(value) = headers
        .get(header::IF_MATCH)
        .and_then(|value| value.to_str().ok())
        && !value
            .split(',')
            .any(|value| value.trim() == etag || value.trim() == "*")
    {
        return true;
    }
    headers
        .get(header::IF_UNMODIFIED_SINCE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| httpdate::parse_http_date(value).ok())
        .is_some_and(|since| system_time(timestamp) > since)
}

fn if_range_matches(value: &str, etag: &str, timestamp: i64) -> bool {
    if value.starts_with('"') {
        return value == etag;
    }
    httpdate::parse_http_date(value)
        .ok()
        .is_some_and(|since| system_time(timestamp) <= since)
}

async fn multipart_response(
    core: Arc<dyn ClientCore>,
    hash: InfoHash,
    index: u32,
    full_length: u64,
    mime: &str,
    ranges: Vec<ByteRange>,
    head: bool,
) -> Result<Response<Body>, Response<Body>> {
    // MatriX.145 uses Go's multipart writer: 30 random bytes, hex-encoded, so
    // a 60-character boundary. The token value is dynamic, but its length is
    // observable through Content-Length and therefore contractual.
    let boundary = format!("{hash}{index:020}");
    let content_type = format!("multipart/byteranges; boundary={boundary}");
    let content_length = ranges
        .iter()
        .map(|range| {
            u64::try_from(
                format!(
                    "--{boundary}\r\nContent-Range: bytes {}-{}/{full_length}\r\nContent-Type: {mime}\r\n\r\n",
                    range.start, range.end
                )
                .len(),
            )
            .expect("multipart header length fits in u64")
                + range.len()
                + 2
        })
        .sum::<u64>()
        + u64::try_from(format!("--{boundary}--\r\n").len())
            .expect("multipart footer length fits in u64");
    if head {
        return Ok(Response::builder()
            .status(StatusCode::PARTIAL_CONTENT)
            .header(header::CONTENT_TYPE, content_type)
            .header(header::CONTENT_LENGTH, content_length)
            .body(Body::empty())
            .unwrap());
    }
    let mime = mime.to_owned();
    let stream_boundary = boundary.clone();
    let stream: std::pin::Pin<
        Box<dyn futures_core::Stream<Item = Result<Bytes, io::Error>> + Send>,
    > = Box::pin(try_stream! {
        for range in ranges {
            yield Bytes::from(format!("--{stream_boundary}\r\nContent-Range: bytes {}-{}/{full_length}\r\nContent-Type: {mime}\r\n\r\n", range.start, range.end));
            let playback = core.clone().playback(PlaybackRequest {
                hash,
                index,
                offset: range.start,
                end: Some(range.end),
                prefetch_offset: None,
            }).await.map_err(io::Error::other)?;
            let mut reader = playback.reader.take(range.len());
            let mut buffer = vec![0; 32 * 1024];
            loop {
                let count = reader.read(&mut buffer).await.map_err(io::Error::other)?;
                if count == 0 { break; }
                yield Bytes::copy_from_slice(&buffer[..count]);
            }
            yield Bytes::from_static(b"\r\n");
        }
        yield Bytes::from(format!("--{stream_boundary}--\r\n"));
    });
    let body = Body::from_stream(stream);
    Ok(Response::builder()
        .status(StatusCode::PARTIAL_CONTENT)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_LENGTH, content_length)
        .body(body)
        .unwrap())
}

async fn playlist_root(
    State(state): State<AppState>,
    request: Request<Body>,
) -> Result<Response<Body>, Response<Body>> {
    let peer = peer(&request);
    playlist_impl(state, peer, None, request).await
}

async fn playlist_named(
    State(state): State<AppState>,
    Path(fname): Path<String>,
    request: Request<Body>,
) -> Result<Response<Body>, Response<Body>> {
    let peer = peer(&request);
    playlist_impl(state, peer, Some(fname), request).await
}

async fn playlist_impl(
    state: AppState,
    peer: IpAddr,
    fname: Option<String>,
    request: Request<Body>,
) -> Result<Response<Body>, Response<Body>> {
    let query = query(request.uri());
    let hash = query
        .get("hash")
        .ok_or_else(|| ApiError::Status(StatusCode::BAD_REQUEST).into_response())?
        .parse::<InfoHash>()
        .map_err(|_| ApiError::Status(StatusCode::BAD_REQUEST).into_response())?;
    let torrent = match state
        .core
        .torrents(TorrentCommand::Add(AddTorrent {
            link: hash.to_string(),
            ..AddTorrent::default()
        }))
        .await
    {
        Ok(TorrentReply::Torrent(Some(torrent))) => *torrent,
        _ => return Err(StatusCode::NOT_FOUND.into_response()),
    };
    let base = state
        .http
        .public_base(peer, request.headers(), request.uri());
    individual_playlist(
        &state,
        &torrent,
        &base,
        fname.as_deref(),
        query.contains_key("fromlast"),
        query.get("index").and_then(|index| index.parse().ok()),
        request.headers(),
    )
    .await
    .map_err(IntoResponse::into_response)
}

async fn individual_playlist(
    state: &AppState,
    torrent: &TorrentView,
    base: &str,
    fname: Option<&str>,
    from_last: bool,
    start_index: Option<u32>,
    headers: &HeaderMap,
) -> Result<Response<Body>, ApiError> {
    let viewed = state
        .core
        .viewed(ViewedCommand::List {
            hash: torrent.hash(),
        })
        .await
        .map_err(lifecycle)?;
    let name = m3u::playlist_name(fname, torrent.name.as_deref().unwrap_or(&torrent.title));
    let hash = torrent.hash.as_deref().unwrap_or_default();
    let etag = m3u::etag(hash, &name);
    if not_modified(headers, &etag, torrent.timestamp) {
        return Ok(Response::builder()
            .status(StatusCode::NOT_MODIFIED)
            .header(header::ETAG, etag)
            .body(Body::empty())
            .unwrap());
    }
    let body = m3u::one(torrent, base, from_last, start_index, &viewed);
    m3u_response(name, etag, torrent.timestamp, body)
}

async fn playlist_all(
    State(state): State<AppState>,
    request: Request<Body>,
) -> Result<Response<Body>, Response<Body>> {
    if !management_authorized(&state, request.headers()) {
        return Err(unauthorized());
    }
    let query = query(request.uri());
    let torrents = match state
        .core
        .torrents(TorrentCommand::List)
        .await
        .map_err(|error| lifecycle(error).into_response())?
    {
        TorrentReply::List(torrents) => torrents,
        _ => Vec::new(),
    };
    let settings = state
        .core
        .settings(SettingsCommand::Get)
        .await
        .map_err(|error| lifecycle(error).into_response())?;
    let base = state
        .http
        .public_base(peer(&request), request.headers(), request.uri());
    let body = m3u::all(
        &torrents,
        &base,
        settings.merge_all_m3u,
        query.get("category").map(String::as_str),
        query.get("search").map(String::as_str),
    );
    let hash = torrents
        .iter()
        .filter_map(|torrent| torrent.hash.as_deref())
        .collect::<String>();
    m3u_response("all.m3u".into(), m3u::etag(&hash, "all.m3u"), 0, body)
        .map_err(IntoResponse::into_response)
}

fn m3u_response(
    name: String,
    etag: String,
    timestamp: i64,
    body: String,
) -> Result<Response<Body>, ApiError> {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "audio/x-mpegurl")
        .header(header::CONNECTION, "close")
        .header(header::ETAG, etag)
        .header(
            header::LAST_MODIFIED,
            httpdate::fmt_http_date(system_time(timestamp)),
        )
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{name}\""),
        )
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_LENGTH, body.len())
        .body(Body::from(body))
        .map_err(|_| ApiError::Status(StatusCode::INTERNAL_SERVER_ERROR))
}

/// gin answers HEAD only where a route registers it (or `Any`); axum lets
/// every GET route answer HEAD.
async fn head_only_where_routed(request: Request<Body>, next: Next) -> Response<Body> {
    if request.method() == Method::HEAD && !head_routed(request.uri().path()) {
        return ApiError::NotFound.into_response();
    }
    next.run(request).await
}

fn head_routed(path: &str) -> bool {
    let under = |prefix: &str| path == prefix || path.starts_with(&format!("{prefix}/"));
    under("/stream")
        || path.starts_with("/play/")
        || under("/dav")
        || under("/mcp")
        || path == "/msx/proxy"
        || path.starts_with("/files/")
}

async fn not_found() -> ApiError {
    ApiError::NotFound
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
pub(crate) mod tests {
    use std::{collections::HashMap, fs};

    use axum::{body::to_bytes, http::Request};
    use rustorr_lifecycle::{InMemoryClientCore, TorrentFileView, ViewedCommand};
    use tower::ServiceExt;

    use super::*;

    #[tokio::test]
    async fn echo_and_reference_404_shape_are_stable() {
        let app = router(ServerInfo {
            version: "MatriX.145".into(),
        });
        let response = app
            .clone()
            .oneshot(Request::builder().uri("/echo").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            to_bytes(response.into_body(), usize::MAX).await.unwrap(),
            "MatriX.145"
        );

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/missing")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(response.headers().get(header::ALLOW).is_none());
    }

    pub(crate) fn playback_app() -> (Router, Arc<InMemoryClientCore>, InfoHash) {
        let (core, hash) = playback_core();
        let client: Arc<dyn ClientCore> = core.clone();
        (
            router_with_core(
                ServerInfo {
                    version: "MatriX.145".into(),
                },
                client,
                HttpConfig::default(),
            ),
            core,
            hash,
        )
    }

    /// One working torrent with a ten-byte `video.mp4`.
    pub(crate) fn playback_core() -> (Arc<InMemoryClientCore>, InfoHash) {
        let hash: InfoHash = "0101010101010101010101010101010101010101".parse().unwrap();
        let core = Arc::new(InMemoryClientCore::new());
        core.insert(
            TorrentView {
                title: "Fixture".into(),
                category: String::new(),
                poster: String::new(),
                data: None,
                timestamp: 1_700_000_000,
                name: Some("video.mp4".into()),
                hash: Some(hash.to_string()),
                torrs_hash: None,
                stat: 3,
                stat_string: "Torrent working".into(),
                loaded_size: None,
                torrent_size: Some(10),
                download_speed: None,
                upload_speed: None,
                total_peers: None,
                active_peers: None,
                connected_seeders: None,
                bytes_written: None,
                bytes_read: None,
                file_stats: vec![TorrentFileView {
                    id: 1,
                    path: "video.mp4".into(),
                    length: 10,
                    engine_index: 0,
                }],
            },
            HashMap::from([(1, (0..10).collect())]),
        )
        .unwrap();
        (core, hash)
    }

    #[tokio::test]
    async fn a_single_file_torrent_plays_whatever_index_is_asked() {
        let (app, _, hash) = playback_app();
        for index in ["1", "9", "x"] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!("/play/{hash}/{index}"))
                        .header(header::RANGE, "bytes=0-1")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT, "{index}");
        }
    }

    #[tokio::test]
    async fn raw_playback_supports_full_single_suffix_multipart_and_unsatisfied_ranges() {
        let (app, _, hash) = playback_app();
        for (range, status, expected) in [
            (None, StatusCode::OK, (0..10).collect::<Vec<_>>()),
            (
                Some("bytes=2-4"),
                StatusCode::PARTIAL_CONTENT,
                vec![2, 3, 4],
            ),
            (Some("bytes=-2"), StatusCode::PARTIAL_CONTENT, vec![8, 9]),
        ] {
            let mut builder = Request::builder().uri(format!("/play/{hash}/1"));
            if let Some(range) = range {
                builder = builder.header(header::RANGE, range);
            }
            let response = app
                .clone()
                .oneshot(builder.body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), status);
            assert_eq!(
                to_bytes(response.into_body(), usize::MAX).await.unwrap(),
                expected
            );
        }

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/play/{hash}/1"))
                    .header(header::RANGE, "bytes=0-1,8-9")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        let content_type = response.headers()[header::CONTENT_TYPE].to_str().unwrap();
        let boundary = content_type
            .strip_prefix("multipart/byteranges; boundary=")
            .unwrap();
        assert_eq!(boundary.len(), 60);
        let content_length = response.headers()[header::CONTENT_LENGTH]
            .to_str()
            .unwrap()
            .parse::<usize>()
            .unwrap();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(body.len(), content_length);
        assert!(body.windows(2).any(|bytes| bytes == [0, 1]));
        assert!(body.windows(2).any(|bytes| bytes == [8, 9]));

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/play/{hash}/1"))
                    .header(header::RANGE, "bytes=99-")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(response.headers()[header::CONTENT_RANGE], "bytes */10");
        assert!(response.headers().get(header::ETAG).is_none());
        assert!(response.headers().get(header::LAST_MODIFIED).is_none());
        assert_eq!(
            to_bytes(response.into_body(), usize::MAX).await.unwrap(),
            "invalid range: failed to overlap\n"
        );
    }

    #[tokio::test]
    async fn head_marks_the_file_viewed_without_returning_a_body() {
        let (app, core, hash) = playback_app();
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::HEAD)
                    .uri(format!("/play/{hash}/1"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            core.viewed(ViewedCommand::List { hash: Some(hash) })
                .await
                .unwrap(),
            [rustorr_lifecycle::ViewedFile {
                hash,
                index: 1,
                timecode: 0.0,
            }]
        );
    }

    #[tokio::test]
    async fn cors_preflight_short_circuits_and_management_routes_require_basic_auth() {
        let (_, core, _) = playback_app();
        let directory = tempfile::tempdir().unwrap();
        let accounts = directory.path().join("accs.db");
        fs::write(&accounts, br#"{"client":"secret"}"#).unwrap();
        let client: Arc<dyn ClientCore> = core;
        let app = router_with_core(
            ServerInfo {
                version: "MatriX.145".into(),
            },
            client,
            HttpConfig {
                credentials: Some(crate::Credentials::read(&accounts).unwrap()),
                ..HttpConfig::default()
            },
        );
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::OPTIONS)
                    .uri("/torrents")
                    .header(header::ORIGIN, "https://client.invalid")
                    .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(response.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");

        let request = || {
            Request::builder()
                .method(Method::POST)
                .uri("/settings")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"action":"get"}"#))
                .unwrap()
        };
        let response = app.clone().oneshot(request()).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let mut authorized = request();
        authorized.headers_mut().insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Basic Y2xpZW50OnNlY3JldA=="),
        );
        let response = app.oneshot(authorized).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
}
