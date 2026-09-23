//! `/gst/*`: MatriX.145's GStreamer HLS module (`server/gstreamer`, built
//! with `-tags gst`). Without a runtime, the server answers as a build
//! without the tag: `GET /gst/settings` → `{"built_in":false}`, `POST` → a
//! JSON `404`, and no other `/gst` route.
//!
//! `/gst/settings` is behind HTTP authentication; the HLS routes are not,
//! as in the reference, where the module mounts them on the root router.

use std::{collections::HashMap, sync::Arc, time::Duration};

use axum::{
    Router,
    body::Body,
    extract::{Path as UrlPath, Query, State},
    http::{HeaderMap, HeaderValue, Response, StatusCode, header},
    response::IntoResponse,
    routing::get,
};
use rustorr_gstreamer::{
    Config, Error,
    env::discover,
    mp4box::Segment,
    playlist,
    service::{BoxFuture, Cancel, ConfigStore, Host, Runtime, Service, Task},
    subtitles,
};
use rustorr_lifecycle::{ClientCore, InfoHash, TorrentCommand, TorrentReply};
use serde_json::Value;

use crate::{
    ApiError,
    app::{AppState, cache_view, json_response, management_authorized, unauthorized},
    go_decode::{self, OrderedObject},
};

/// What the server provides to run the module: the GStreamer runtime and
/// where its settings are stored.
#[derive(Clone)]
pub struct GstreamerSetup {
    pub runtime: Arc<dyn Runtime>,
    pub store: Arc<dyn ConfigStore>,
}

/// Builds the module's service; the inactive-task sweep runs every minute
/// while the service lives.
pub(crate) fn build_service(
    setup: &GstreamerSetup,
    core: Arc<dyn ClientCore>,
    port: u16,
) -> Arc<Service> {
    let host = Arc::new(HttpHost {
        core,
        port,
        client: reqwest::Client::new(),
    });
    let service = Service::new(setup.store.clone(), host, setup.runtime.clone());
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        let weak = Arc::downgrade(&service);
        handle.spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(60));
            ticker.tick().await;
            loop {
                ticker.tick().await;
                let Some(service) = weak.upgrade() else {
                    break;
                };
                let _ = tokio::task::spawn_blocking(move || service.cleanup_inactive()).await;
            }
        });
    }
    service
}

/// The HLS routes, mounted only with a runtime.
pub(crate) fn routes(router: Router<AppState>) -> Router<AppState> {
    router
        .route("/gst/remove", get(remove))
        .route("/gst/echo", get(echo))
        .route("/gst/{hash}/heartbeat", get(heartbeat))
        .route("/gst/{hash}/probe", get(probe))
        .route("/gst/{hash}/master.m3u8", get(master))
        .route("/gst/{hash}/video.m3u8", get(video_playlist))
        .route("/gst/{hash}/init.mp4", get(init_mp4))
        // gin's `/seg/*segment` also matches an empty remainder.
        .route("/gst/{hash}/seg/", get(segment_root))
        .route("/gst/{hash}/seg/{*segment}", get(segment))
        .route("/gst/{hash}/subs/", get(subtitle_root))
        .route("/gst/{hash}/subs/{*subtitle}", get(subtitle))
}

/// Removes the torrent's HLS task, as `/torrents` `rem`, `drop` and `wipe`
/// do in the reference.
pub(crate) fn remove_task(state: &AppState, hash: &str) {
    if let Some(service) = &state.gstreamer {
        service.try_remove(hash);
    }
}

struct HttpHost {
    core: Arc<dyn ClientCore>,
    port: u16,
    client: reqwest::Client,
}

impl HttpHost {
    async fn torrent(&self, hash: &str) -> Option<rustorr_lifecycle::TorrentView> {
        let hash: InfoHash = hash.parse().ok()?;
        match self.core.torrents(TorrentCommand::Get(hash)).await {
            Ok(TorrentReply::Torrent(Some(torrent))) => Some(*torrent),
            _ => None,
        }
    }
}

impl Host for HttpHost {
    fn port(&self) -> u16 {
        self.port
    }

    fn file_size(&self, hash: &str, file_id: &str) -> BoxFuture<'_, Option<i64>> {
        let hash = hash.to_string();
        let index = file_id.parse::<u32>().ok().filter(|index| *index > 0);
        Box::pin(async move {
            let index = index?;
            let torrent = self.torrent(&hash).await?;
            torrent
                .file_stats
                .iter()
                .find(|file| file.id == index && file.length > 0)
                .map(|file| file.length as i64)
        })
    }

    fn heartbeat(&self, hash: &str) -> BoxFuture<'_, Value> {
        let hash = hash.to_string();
        Box::pin(async move {
            let bare = serde_json::json!({ "Hash": hash });
            let Ok(info_hash) = hash.parse::<InfoHash>() else {
                return bare;
            };
            if let Ok(view) = cache_view(self.core.as_ref(), info_hash).await
                && let Ok(value) = serde_json::to_value(view)
            {
                return value;
            }
            match self.torrent(&hash).await {
                Some(torrent) => serde_json::json!({ "Hash": hash, "Torrent": torrent }),
                None => bare,
            }
        })
    }

    fn drop_torrent(&self, hash: &str) -> BoxFuture<'_, ()> {
        let hash = hash.parse::<InfoHash>();
        Box::pin(async move {
            if let Ok(hash) = hash {
                let _ = self.core.torrents(TorrentCommand::Drop(hash)).await;
            }
        })
    }

    fn discover(&self, url: &str, config: &Config) -> BoxFuture<'_, (String, Option<Error>)> {
        let (url, config) = (url.to_string(), config.clone());
        Box::pin(async move { discover(&url, &config).await })
    }

    fn read_range(&self, url: &str, offset: u64, length: u64) -> BoxFuture<'_, Option<Vec<u8>>> {
        let url = url.to_string();
        Box::pin(async move {
            if length == 0 {
                return None;
            }
            let response = self
                .client
                .get(&url)
                .header("Accept-Encoding", "identity")
                .header("Range", format!("bytes={offset}-{}", offset + length - 1))
                .send()
                .await
                .ok()?;
            let status = response.status();
            if status != StatusCode::PARTIAL_CONTENT && !(offset == 0 && status == StatusCode::OK) {
                return None;
            }
            if status == StatusCode::PARTIAL_CONTENT {
                let range = response
                    .headers()
                    .get(header::CONTENT_RANGE)?
                    .to_str()
                    .ok()?;
                if !range.starts_with(&format!("bytes {offset}-")) {
                    return None;
                }
            }
            let body = response.bytes().await.ok()?;
            // io.ReadFull: exactly `length` bytes or nothing.
            (body.len() as u64 >= length).then(|| body[..length as usize].to_vec())
        })
    }
}

/// Cancels the blocking work of a request when its future is dropped, which
/// is what happens when the client goes away.
struct CancelOnDrop(Cancel);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

async fn blocking<T: Send + 'static>(work: impl FnOnce(&Cancel) -> T + Send + 'static) -> T {
    let guard = CancelOnDrop(Cancel::default());
    let cancel = guard.0.clone();
    let result = tokio::task::spawn_blocking(move || work(&cancel))
        .await
        .expect("a GStreamer request does not panic");
    drop(guard);
    result
}

fn empty(status: StatusCode) -> Response<Body> {
    status.into_response()
}

/// `noCache`.
fn no_cache(mut response: Response<Body>) -> Response<Body> {
    let headers = response.headers_mut();
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, no-cache, must-revalidate, max-age=0"),
    );
    headers.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    headers.insert(header::EXPIRES, HeaderValue::from_static("0"));
    response
}

fn data(content_type: &'static str, body: impl Into<Body>) -> Response<Body> {
    ([(header::CONTENT_TYPE, content_type)], body.into()).into_response()
}

const PLAYLIST: &str = "application/vnd.apple.mpegurl; charset=utf-8";

/// `abortWithSourceError`: the error text, `504` after a timeout.
fn source_error(error: &Error) -> Response<Body> {
    let status = if *error == Error::DeadlineExceeded {
        StatusCode::GATEWAY_TIMEOUT
    } else {
        StatusCode::BAD_GATEWAY
    };
    (
        status,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        error.to_string(),
    )
        .into_response()
}

/// `abortWithRequestError`: a bare status, nothing when the client left.
fn request_error(error: &Error) -> Response<Body> {
    if *error == Error::Canceled {
        return empty(StatusCode::OK);
    }
    empty(StatusCode::BAD_GATEWAY)
}

fn log_failure(prefix: &str, operation: &str, error: &Error) {
    if *error == Error::Canceled {
        tracing::debug!("[GStreamer] debug: {prefix} {operation} canceled");
    } else {
        tracing::error!("[GStreamer] error: {prefix} {operation} failed: {error}");
    }
}

fn task_prefix(task: &Task) -> String {
    format!(
        "hash={} file={} audio={}",
        task.info.id, task.info.file_id, task.info.audio
    )
}

type QueryMap = Query<HashMap<String, String>>;

fn first_non_empty(query: &HashMap<String, String>, keys: &[&str]) -> String {
    keys.iter()
        .filter_map(|key| query.get(*key))
        .find(|value| !value.is_empty())
        .cloned()
        .unwrap_or_default()
}

/// `parseQueryInt`: `strconv.Atoi`, else the fallback.
fn query_int(query: &HashMap<String, String>, key: &str, fallback: i64) -> i64 {
    match query.get(key).filter(|value| !value.is_empty()) {
        Some(value) => value.parse().unwrap_or(fallback),
        None => fallback,
    }
}

fn service(state: &AppState) -> &Arc<Service> {
    state
        .gstreamer
        .as_ref()
        .expect("the HLS routes are mounted only with a runtime")
}

pub(crate) async fn get_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response<Body>, Response<Body>> {
    if !management_authorized(&state, &headers) {
        return Err(unauthorized());
    }
    let Some(service) = state.gstreamer.clone() else {
        return json_response(serde_json::json!({ "built_in": false }))
            .map_err(IntoResponse::into_response);
    };
    let config = tokio::task::spawn_blocking(move || service.current_config())
        .await
        .map_err(|_| empty(StatusCode::INTERNAL_SERVER_ERROR))?;
    json_response(serde_json::json!({
        "built_in": true,
        "config": config,
        "defaults": Config::platform_defaults().normalized(),
    }))
    .map_err(IntoResponse::into_response)
}

pub(crate) async fn set_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Response<Body>, Response<Body>> {
    if !management_authorized(&state, &headers) {
        return Err(unauthorized());
    }
    let json_error =
        |status: StatusCode, message: String| ApiError::Json { status, message }.into_response();
    let Some(service) = state.gstreamer.clone() else {
        return Err(json_error(
            StatusCode::NOT_FOUND,
            "gstreamer is not built in".into(),
        ));
    };
    let request =
        settings_request(&body).map_err(|message| json_error(StatusCode::BAD_REQUEST, message))?;
    let ok = || json_response(serde_json::json!({ "status": "ok" }));
    let read_only = state.http.read_only;
    let save = move |config: Config| {
        let service = service.clone();
        async move {
            if read_only {
                return Err("read-only mode".to_string());
            }
            tokio::task::spawn_blocking(move || service.save_config(config))
                .await
                .map_err(|error| error.to_string())?
        }
    };
    match request.action.as_str() {
        "def" => {
            save(Config::platform_defaults())
                .await
                .map_err(|message| json_error(StatusCode::FORBIDDEN, message))?;
            ok().map_err(IntoResponse::into_response)
        }
        "set" | "" => {
            let Some(config) = request.config else {
                return Err(json_error(
                    StatusCode::BAD_REQUEST,
                    "config is required".into(),
                ));
            };
            save(config).await.map_err(|message| {
                let status = if message == "read-only mode" {
                    StatusCode::FORBIDDEN
                } else {
                    StatusCode::INTERNAL_SERVER_ERROR
                };
                json_error(status, message)
            })?;
            ok().map_err(IntoResponse::into_response)
        }
        _ => Err(json_error(StatusCode::BAD_REQUEST, "unknown action".into())),
    }
}

struct SettingsRequest {
    action: String,
    config: Option<Config>,
}

/// `ShouldBindJSON` into `gstreamerSettingsRequest`: case-insensitive keys,
/// `null` leaves a field alone (and clears the config pointer), and the
/// first type mismatch is reported in `encoding/json`'s words.
fn settings_request(input: &[u8]) -> Result<SettingsRequest, String> {
    const REQUEST: &str = "gstreamerSettingsRequest";
    let value = go_decode::first_value(input)?;
    let mut request = SettingsRequest {
        action: String::new(),
        config: None,
    };
    match value {
        Value::Null => return Ok(request),
        Value::Object(_) => {}
        other => {
            return Err(format!(
                "json: cannot unmarshal {} into Go value of type api.{REQUEST}",
                go_decode::kind(&other)
            ));
        }
    }
    let OrderedObject(members) = go_decode::ordered(input)?;
    let mut error = None;
    for (key, value) in members {
        let field = if key == "action" || key.eq_ignore_ascii_case("action") {
            "action"
        } else if key == "config" || key.eq_ignore_ascii_case("config") {
            "config"
        } else {
            continue;
        };
        match (field, value) {
            (_, Value::Null) if field == "action" => {}
            ("action", Value::String(action)) => request.action = action,
            ("action", other) => {
                error.get_or_insert_with(|| {
                    format!(
                        "json: cannot unmarshal {} into Go struct field {REQUEST}.action of type string",
                        go_decode::kind(&other)
                    )
                });
            }
            (_, Value::Null) => request.config = None,
            (_, Value::Object(members)) => {
                let config = request.config.get_or_insert_with(Config::default);
                if let Err(message) = merge_config(config, members) {
                    error.get_or_insert(message);
                }
            }
            (_, other) => {
                error.get_or_insert_with(|| {
                    format!(
                        "json: cannot unmarshal {} into Go struct field {REQUEST}.config of type gstreamer.Config",
                        go_decode::kind(&other)
                    )
                });
            }
        }
    }
    error.map_or(Ok(request), Err)
}

#[derive(Clone, Copy)]
enum Kind {
    Bool,
    Int,
    Float,
    String,
}

/// Decodes members into `config` field by field, as `encoding/json` does
/// into an existing struct.
fn merge_config(
    config: &mut Config,
    members: serde_json::Map<String, Value>,
) -> Result<(), String> {
    let Value::Object(mut fields) = serde_json::to_value(&*config).map_err(|e| e.to_string())?
    else {
        return Ok(());
    };
    let kinds: Vec<(String, Kind)> = fields
        .iter()
        .map(|(name, value)| {
            let kind = match (name.as_str(), value) {
                ("GSTVersion", _) => Kind::Float,
                (_, Value::Bool(_)) => Kind::Bool,
                (_, Value::String(_)) => Kind::String,
                _ => Kind::Int,
            };
            (name.clone(), kind)
        })
        .collect();
    let mut error = None;
    for (key, value) in members {
        let Some((name, kind)) = kinds.iter().find(|(name, _)| *name == key).or_else(|| {
            kinds
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(&key))
        }) else {
            continue;
        };
        if value.is_null() {
            continue;
        }
        let mismatch = |value: &Value| {
            let go_type = match kind {
                Kind::Bool => "bool",
                Kind::Int => "int",
                Kind::Float => "float64",
                Kind::String => "string",
            };
            // A number is quoted only where its text failed to parse.
            let found = match (kind, value) {
                (Kind::Int | Kind::Float, Value::Number(number)) => format!("number {number}"),
                (_, other) => go_decode::kind(other).to_string(),
            };
            format!(
                "json: cannot unmarshal {found} into Go struct field Config.config.{name} of type {go_type}"
            )
        };
        let accepted = match (kind, &value) {
            (Kind::Bool, Value::Bool(_)) | (Kind::String, Value::String(_)) => true,
            (Kind::Int, Value::Number(number)) => number.is_i64(),
            (Kind::Float, Value::Number(number)) => number.as_f64().is_some_and(f64::is_finite),
            _ => false,
        };
        if accepted {
            fields.insert(name.clone(), value);
        } else {
            error.get_or_insert_with(|| mismatch(&value));
        }
    }
    if let Ok(merged) = serde_json::from_value(Value::Object(fields)) {
        *config = merged;
    }
    error.map_or(Ok(()), Err)
}

async fn remove(State(state): State<AppState>, query: QueryMap) -> Response<Body> {
    let id = first_non_empty(&query, &["hash", "id"]);
    if id.is_empty() {
        return empty(StatusCode::BAD_REQUEST);
    }
    let service = service(&state).clone();
    let removed = {
        let id = id.clone();
        let service = service.clone();
        tokio::task::spawn_blocking(move || service.try_remove(&id))
            .await
            .unwrap_or(false)
    };
    if !removed {
        return empty(StatusCode::NOT_FOUND);
    }
    service.host().drop_torrent(&id).await;
    json_response(serde_json::json!({ "success": true }))
        .unwrap_or_else(IntoResponse::into_response)
}

async fn echo(State(state): State<AppState>) -> Response<Body> {
    json_response(service(&state).echo().await).unwrap_or_else(IntoResponse::into_response)
}

async fn heartbeat(
    State(state): State<AppState>,
    UrlPath(hash): UrlPath<String>,
) -> Response<Body> {
    let service = service(&state);
    if service.get(&hash).is_none() {
        return empty(StatusCode::NOT_FOUND);
    }
    json_response(service.host().heartbeat(&hash).await).unwrap_or_else(IntoResponse::into_response)
}

async fn probe(
    State(state): State<AppState>,
    UrlPath(hash): UrlPath<String>,
    query: QueryMap,
) -> Response<Body> {
    let file_id = first_non_empty(&query, &["index", "id", "fileID"]);
    if file_id.is_empty() {
        return no_cache(empty(StatusCode::BAD_REQUEST));
    }
    no_cache(match service(&state).probe(&hash, &file_id).await {
        Ok(probe) => json_response(probe).unwrap_or_else(IntoResponse::into_response),
        Err(error) => {
            log_failure(
                &format!("hash={hash} file={file_id} audio=0"),
                "probe request",
                &error,
            );
            source_error(&error)
        }
    })
}

async fn master(
    State(state): State<AppState>,
    UrlPath(hash): UrlPath<String>,
    query: QueryMap,
) -> Response<Body> {
    let file_id = first_non_empty(&query, &["index", "id", "fileID"]);
    let audio = query_int(&query, "audio", 0);
    let task = match service(&state).get_or_add(&hash, &file_id, audio).await {
        Ok(task) => task,
        Err(error) => {
            log_failure(
                &format!("hash={hash} file={file_id} audio={audio}"),
                "master task creation",
                &error,
            );
            return no_cache(source_error(&error));
        }
    };
    let seconds = query_int(&query, "seconds", 0);
    let start = task.info.start_index_for_seconds(seconds);
    let init_task = task.clone();
    if let Err(error) = blocking(move |cancel| init_task.ensure_init(cancel, audio, start)).await {
        log_failure(&task_prefix(&task), "master init", &error);
        return no_cache(request_error(&error));
    }
    let view = task.info.media(audio);
    no_cache(data(PLAYLIST, playlist::master(&view.media(), seconds)))
}

async fn video_playlist(
    State(state): State<AppState>,
    UrlPath(hash): UrlPath<String>,
    query: QueryMap,
) -> Response<Body> {
    let Some(task) = service(&state).get(&hash) else {
        return no_cache(empty(StatusCode::NOT_FOUND));
    };
    let audio = query_int(&query, "audio", task.info.audio);
    let start = task
        .info
        .start_index_for_seconds(query_int(&query, "seconds", 0));
    let view = task.info.media(audio);
    no_cache(data(
        PLAYLIST,
        playlist::media_playlist(&view.media(), start, audio),
    ))
}

async fn init_mp4(
    State(state): State<AppState>,
    UrlPath(hash): UrlPath<String>,
    query: QueryMap,
) -> Response<Body> {
    let Some(task) = service(&state).get(&hash) else {
        return no_cache(empty(StatusCode::NOT_FOUND));
    };
    let audio = query_int(&query, "audio", task.info.audio);
    let start = task
        .info
        .start_index_for_seconds(query_int(&query, "seconds", 0));
    let init_task = task.clone();
    if let Err(error) = blocking(move |cancel| init_task.ensure_init(cancel, audio, start)).await {
        log_failure(&task_prefix(&task), "init.mp4 preparation", &error);
        return no_cache(request_error(&error));
    }
    match task.info.init_data() {
        Some(init) => no_cache(data("video/mp4", init.as_ref().clone())),
        None => {
            log_failure(
                &task_prefix(&task),
                "init.mp4 response",
                &Error::SegmentNotReady,
            );
            no_cache(request_error(&Error::SegmentNotReady))
        }
    }
}

async fn segment_root(
    state: State<AppState>,
    hash: UrlPath<String>,
    query: QueryMap,
    headers: HeaderMap,
) -> Response<Body> {
    serve_segment(state, hash.0, String::new(), query, headers).await
}

async fn segment(
    state: State<AppState>,
    UrlPath((hash, segment)): UrlPath<(String, String)>,
    query: QueryMap,
    headers: HeaderMap,
) -> Response<Body> {
    serve_segment(state, hash, segment, query, headers).await
}

/// `parseSegmentIndex`.
fn segment_index(value: &str) -> Option<i64> {
    let value = value.strip_prefix('/').unwrap_or(value);
    let value = value.strip_suffix(".m4s").unwrap_or(value);
    if value.is_empty() || value.contains('/') {
        return None;
    }
    value.parse::<i64>().ok().filter(|index| *index >= 0)
}

async fn serve_segment(
    State(state): State<AppState>,
    hash: String,
    segment: String,
    Query(query): QueryMap,
    headers: HeaderMap,
) -> Response<Body> {
    let Some(task) = service(&state).get(&hash) else {
        return no_cache(empty(StatusCode::NOT_FOUND));
    };
    let Some(index) = segment_index(&segment) else {
        return no_cache(empty(StatusCode::BAD_REQUEST));
    };
    let audio = query_int(&query, "audio", task.info.audio);
    let work_task = task.clone();
    let result = blocking(move |cancel| {
        if !work_task.info.has_init() {
            work_task
                .ensure_init(cancel, audio, index)
                .map_err(|error| (format!("segment {index} init"), error))?;
        }
        let segment = work_task
            .segment(cancel, index, audio)
            .map_err(|error| (format!("segment {index} response"), error))?;
        if segment.data.is_empty() {
            return Err((format!("segment {index} response"), Error::SegmentNotReady));
        }
        Ok(segment)
    })
    .await;
    match result {
        Ok(segment) => no_cache(write_segment(segment, &headers)),
        Err((operation, error)) => {
            log_failure(&task_prefix(&task), &operation, &error);
            no_cache(request_error(&error))
        }
    }
}

/// `writeSegment`: the whole segment, or one byte range of it.
fn write_segment(segment: Segment, headers: &HeaderMap) -> Response<Body> {
    let total = segment.data.len() as u64;
    let base = [
        (header::CONTENT_TYPE, "video/mp4".to_string()),
        (header::ACCEPT_RANGES, "bytes".to_string()),
    ];
    let Some(range) = headers.get(header::RANGE) else {
        return (base, segment.data).into_response();
    };
    match single_range(range.to_str().unwrap_or_default(), total) {
        Some((start, end)) => (
            StatusCode::PARTIAL_CONTENT,
            base,
            [(
                header::CONTENT_RANGE,
                format!("bytes {start}-{end}/{total}"),
            )],
            segment.data[start as usize..=end as usize].to_vec(),
        )
            .into_response(),
        None => (
            StatusCode::RANGE_NOT_SATISFIABLE,
            base,
            [(header::CONTENT_RANGE, format!("bytes */{total}"))],
        )
            .into_response(),
    }
}

/// `parseSingleRange`.
fn single_range(value: &str, total: u64) -> Option<(u64, u64)> {
    let spec = value.strip_prefix("bytes=")?.trim();
    if total == 0 || spec.is_empty() || spec.contains(',') {
        return None;
    }
    let (left, right) = spec.split_once('-')?;
    let total = total as i64;
    let (start, mut end) = if left.is_empty() {
        let suffix: i64 = right.parse().ok().filter(|suffix| *suffix > 0)?;
        (total - suffix.min(total), total - 1)
    } else {
        let start: i64 = left.parse().ok()?;
        let end = if right.is_empty() {
            total - 1
        } else {
            right.parse().ok()?
        };
        (start, end)
    };
    if start < 0 || end < start || start >= total {
        return None;
    }
    end = end.min(total - 1);
    Some((start as u64, end as u64))
}

async fn subtitle_root(state: State<AppState>, hash: UrlPath<String>) -> Response<Body> {
    serve_subtitle(state, hash.0, String::new()).await
}

async fn subtitle(
    state: State<AppState>,
    UrlPath((hash, path)): UrlPath<(String, String)>,
) -> Response<Body> {
    serve_subtitle(state, hash, path).await
}

async fn serve_subtitle(
    State(state): State<AppState>,
    hash: String,
    path: String,
) -> Response<Body> {
    let Some(task) = service(&state).get(&hash) else {
        return no_cache(empty(StatusCode::NOT_FOUND));
    };
    let path = path.strip_prefix('/').unwrap_or(&path);
    if let Some(track) = path.strip_suffix(".m3u8")
        && !track.contains('/')
    {
        let Some(track) = track.parse::<i64>().ok().filter(|track| *track >= 0) else {
            return no_cache(empty(StatusCode::BAD_REQUEST));
        };
        let view = task.info.media(task.info.audio);
        return no_cache(data(
            PLAYLIST,
            playlist::subtitle_playlist(&view.media(), track),
        ));
    }
    let parts: Vec<&str> = path.split('/').collect();
    let [track, file] = parts[..] else {
        return no_cache(empty(StatusCode::BAD_REQUEST));
    };
    let Some(segment) = file.strip_suffix(".vtt") else {
        return no_cache(empty(StatusCode::BAD_REQUEST));
    };
    let (Ok(track), Ok(segment)) = (track.parse::<i64>(), segment.parse::<i64>()) else {
        return no_cache(empty(StatusCode::BAD_REQUEST));
    };
    if track < 0 || segment < 0 {
        return no_cache(empty(StatusCode::BAD_REQUEST));
    }
    let vtt = task
        .subtitle_vtt(track, segment, subtitles::WAIT_TIMEOUT)
        .await;
    no_cache(data("text/vtt; charset=utf-8", vtt))
}

#[cfg(test)]
mod tests {
    use axum::{
        body::to_bytes,
        http::{Method, Request},
    };
    use tower::ServiceExt;

    use super::*;
    use crate::app::tests::playback_app;

    async fn call(method: Method, uri: &str, body: &str) -> (StatusCode, String) {
        let (app, _, _) = playback_app();
        let response = app
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_owned()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, String::from_utf8(body.to_vec()).unwrap())
    }

    #[tokio::test]
    async fn settings_answer_like_a_build_without_gstreamer() {
        assert_eq!(
            call(Method::GET, "/gst/settings", "").await,
            (StatusCode::OK, r#"{"built_in":false}"#.into())
        );
        assert_eq!(
            call(Method::POST, "/gst/settings", r#"{"action":"def"}"#).await,
            (
                StatusCode::NOT_FOUND,
                r#"{"error":"gstreamer is not built in"}"#.into()
            )
        );
        for path in ["/gst/echo", "/gst/remove?hash=x", "/gst/abc/probe?index=1"] {
            assert_eq!(
                call(Method::GET, path, "").await.0,
                StatusCode::NOT_FOUND,
                "{path}"
            );
        }
    }

    #[test]
    fn settings_requests_decode_like_encoding_json() {
        let request =
            settings_request(br#"{"Action":"set","config":{"segmentseconds":4,"Source":"play"}}"#)
                .unwrap();
        assert_eq!(request.action, "set");
        let config = request.config.unwrap();
        assert_eq!(
            (config.segment_seconds, config.source.as_str()),
            (4, "play")
        );
        assert!(!config.hardware_acceleration);
        for (body, error) in [
            (
                r#"{"action":"set","config":{"MaxTasks":"two"}}"#,
                "json: cannot unmarshal string into Go struct field Config.config.MaxTasks of type int",
            ),
            (
                r#"{"config":"x"}"#,
                "json: cannot unmarshal string into Go struct field gstreamerSettingsRequest.config of type gstreamer.Config",
            ),
            (
                r#"{"action":1}"#,
                "json: cannot unmarshal number into Go struct field gstreamerSettingsRequest.action of type string",
            ),
            (
                r#"{"config":{"MaxTasks":1.5}}"#,
                "json: cannot unmarshal number 1.5 into Go struct field Config.config.MaxTasks of type int",
            ),
            (
                r#"{"config":{"Subtitles":1}}"#,
                "json: cannot unmarshal number into Go struct field Config.config.Subtitles of type bool",
            ),
            (
                "[]",
                "json: cannot unmarshal array into Go value of type api.gstreamerSettingsRequest",
            ),
            ("", "EOF"),
        ] {
            assert_eq!(
                settings_request(body.as_bytes()).err().as_deref(),
                Some(error),
                "{body}"
            );
        }
        assert!(settings_request(b"null").unwrap().config.is_none());
        assert!(
            settings_request(br#"{"config":{"MaxTasks":1},"config":null}"#)
                .unwrap()
                .config
                .is_none()
        );
    }

    struct FakeRuntime;

    impl Runtime for FakeRuntime {
        fn runner(
            &self,
            _task: Arc<rustorr_gstreamer::service::TaskInfo>,
            _audio: i64,
        ) -> Result<Box<dyn rustorr_gstreamer::service::Runner>, Error> {
            Err(Error::PipelineUnavailable("no pipelines in tests".into()))
        }

        fn status(&self, _config: &Config) -> rustorr_gstreamer::env::ComponentStatus {
            rustorr_gstreamer::env::ComponentStatus {
                found: true,
                available: true,
                works: true,
                version: "1.24.2".into(),
                error: String::new(),
            }
        }

        fn config_version(&self, _config: &Config) -> Option<f64> {
            Some(1.24)
        }

        fn hdr_tone_mapping(
            &self,
            _gstreamer: &rustorr_gstreamer::env::ComponentStatus,
        ) -> rustorr_gstreamer::env::ComponentStatus {
            rustorr_gstreamer::env::ComponentStatus::default()
        }
    }

    #[derive(Default)]
    struct MemoryStore(std::sync::Mutex<Option<String>>);

    impl ConfigStore for MemoryStore {
        fn load(&self) -> Option<String> {
            self.0.lock().unwrap().clone()
        }

        fn save(&self, document: &str) -> Result<(), String> {
            *self.0.lock().unwrap() = Some(document.into());
            Ok(())
        }
    }

    async fn module_call(
        read_only: bool,
        store: Arc<MemoryStore>,
        method: Method,
        uri: &str,
        body: &str,
    ) -> (StatusCode, String) {
        let (_, core, _) = playback_app();
        let integrations = crate::Integrations {
            gstreamer: Some(GstreamerSetup {
                runtime: Arc::new(FakeRuntime),
                store,
            }),
            ..crate::Integrations::default()
        };
        let http = crate::HttpConfig {
            read_only,
            ..crate::HttpConfig::default()
        };
        let app = crate::router_with_services(
            crate::ServerInfo {
                version: "test".into(),
            },
            core,
            integrations,
            http,
        );
        let response = app
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_owned()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, String::from_utf8(body.to_vec()).unwrap())
    }

    #[tokio::test]
    async fn settings_are_saved_normalized_and_reported_with_the_runtime_version() {
        let store = Arc::new(MemoryStore::default());
        let set = r#"{"action":"set","config":{"SegmentSeconds":4,"Source":" PLAY "}}"#;
        assert_eq!(
            module_call(false, store.clone(), Method::POST, "/gst/settings", set).await,
            (StatusCode::OK, r#"{"status":"ok"}"#.into())
        );
        let saved: Value = serde_json::from_str(store.load().as_deref().unwrap()).unwrap();
        assert_eq!(saved["Source"], "play");
        assert_eq!(saved["InactiveMinutes"], 5);
        // A new service reads the stored settings back.
        let (status, body) =
            module_call(false, store.clone(), Method::GET, "/gst/settings", "").await;
        assert_eq!(status, StatusCode::OK);
        let body: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(body["built_in"], true);
        assert_eq!(body["config"]["SegmentSeconds"], 4);
        assert_eq!(body["config"]["GSTVersion"], 1.24);
        assert_eq!(body["defaults"]["GSTVersion"], 1.22);
        assert_eq!(body["defaults"]["SegmentSeconds"], 6);
        for request in [set, r#"{"action":"def"}"#] {
            assert_eq!(
                module_call(true, store.clone(), Method::POST, "/gst/settings", request).await,
                (
                    StatusCode::FORBIDDEN,
                    r#"{"error":"read-only mode"}"#.into()
                )
            );
        }
    }

    #[tokio::test]
    async fn hls_routes_answer_without_a_task() {
        let store = Arc::new(MemoryStore::default());
        let hash = "0000000000000000000000000000000000000000";
        for path in [
            format!("/gst/{hash}/heartbeat"),
            format!("/gst/{hash}/video.m3u8"),
            format!("/gst/{hash}/init.mp4"),
            format!("/gst/{hash}/seg/0.m4s"),
            format!("/gst/{hash}/subs/0.m3u8"),
            format!("/gst/remove?hash={hash}"),
        ] {
            assert_eq!(
                module_call(false, store.clone(), Method::GET, &path, "").await,
                (StatusCode::NOT_FOUND, String::new()),
                "{path}"
            );
        }
        assert_eq!(
            module_call(false, store.clone(), Method::GET, "/gst/remove", "")
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            module_call(
                false,
                store,
                Method::GET,
                &format!("/gst/{hash}/master.m3u8"),
                ""
            )
            .await,
            (StatusCode::BAD_GATEWAY, "bad gstreamer source".into())
        );
    }

    #[test]
    fn segment_indexes_and_ranges_parse_like_the_reference() {
        assert_eq!(segment_index("/3.m4s"), Some(3));
        assert_eq!(segment_index("3"), Some(3));
        assert_eq!(segment_index(""), None);
        assert_eq!(segment_index("a/3.m4s"), None);
        assert_eq!(segment_index("-1.m4s"), None);
        assert_eq!(single_range("bytes=8-39", 100), Some((8, 39)));
        assert_eq!(single_range("bytes=90-", 100), Some((90, 99)));
        assert_eq!(single_range("bytes=-10", 100), Some((90, 99)));
        assert_eq!(single_range("bytes=50-200", 100), Some((50, 99)));
        assert_eq!(single_range("bytes=100-", 100), None);
        assert_eq!(single_range("bytes=0-1,4-5", 100), None);
    }
}
