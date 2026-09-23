//! Routes MatriX.145's web interface uses besides the client API: the magnet
//! list, the speed-test download, the status page and shutdown.

use async_stream::stream;
use axum::{
    body::{Body, Bytes},
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, Request, Response, StatusCode, header},
    response::IntoResponse,
};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use rustorr_lifecycle::{MagnetView, TorrentCommand, TorrentReply, TorrentView};

use crate::{
    ApiError,
    app::{AppState, lifecycle, management_authorized, unauthorized},
    range::{self, ByteRange, RangeError},
};

const MIB: u64 = 1024 * 1024;
static ZEROS: [u8; 64 * 1024] = [0; 64 * 1024];
/// Go's `url.QueryEscape` keeps only these besides ASCII letters and digits.
const QUERY: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');

pub(crate) async fn magnets(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response<Body>, Response<Body>> {
    if !management_authorized(&state, &headers) {
        return Err(unauthorized());
    }
    let TorrentReply::Magnets(magnets) = state
        .core
        .torrents(TorrentCommand::Magnets)
        .await
        .map_err(|error| lifecycle(error).into_response())?
    else {
        return Err(ApiError::Status(StatusCode::INTERNAL_SERVER_ERROR).into_response());
    };
    let mut html = String::from("<div>");
    for magnet in &magnets {
        html.push_str(&format!(
            "<p><a href='{}'>magnet:?xt=urn:btih:{}</a></p>",
            magnet_uri(magnet),
            magnet.hash
        ));
    }
    html.push_str("</div>");
    Ok(([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response())
}

/// anacrolix `Magnet.String()`: the info hash first, then `url.Values` in
/// key order (`dn` before `tr`), each value query-escaped.
fn magnet_uri(magnet: &MagnetView) -> String {
    let escape = |value: &str| {
        utf8_percent_encode(value, QUERY)
            .to_string()
            .replace("%20", "+")
    };
    let mut values = Vec::new();
    if !magnet.name.is_empty() {
        values.push(format!("dn={}", escape(&magnet.name)));
    }
    values.extend(
        magnet
            .trackers
            .iter()
            .map(|tracker| format!("tr={}", escape(tracker))),
    );
    // anacrolix always sets an empty `ws` parameter, so the separator is
    // written even when nothing follows it.
    format!("magnet:?xt=urn:btih:{}&{}", magnet.hash, values.join("&"))
}

pub(crate) async fn download(
    State(state): State<AppState>,
    Path(size): Path<String>,
    request: Request<Body>,
) -> Result<Response<Body>, Response<Body>> {
    if !management_authorized(&state, request.headers()) {
        return Err(unauthorized());
    }
    // gin records the parse error without writing a response: an empty 200.
    let Ok(size) = size.parse::<i32>() else {
        return Ok(StatusCode::OK.into_response());
    };
    let length = u64::try_from(size).unwrap_or(0) * MIB;
    let modified = httpdate::fmt_http_date(std::time::SystemTime::now());
    let ranges = request
        .headers()
        .get(header::RANGE)
        .and_then(|value| value.to_str().ok())
        .map(|value| range::parse(value, length));
    let mut response = match ranges {
        None => zeros_response(StatusCode::OK, length),
        Some(Err(RangeError::Invalid | RangeError::Unsatisfiable)) => {
            let mut response = (
                StatusCode::RANGE_NOT_SATISFIABLE,
                "invalid range: failed to overlap\n",
            )
                .into_response();
            let headers = response.headers_mut();
            headers.insert(
                header::CONTENT_RANGE,
                HeaderValue::from_str(&format!("bytes */{length}")).expect("valid header"),
            );
            headers.insert(
                header::HeaderName::from_static("x-content-type-options"),
                HeaderValue::from_static("nosniff"),
            );
            return Ok(response);
        }
        Some(Ok(ranges)) if ranges.len() == 1 => {
            let range = ranges[0];
            let mut response = zeros_response(StatusCode::PARTIAL_CONTENT, range.len());
            response.headers_mut().insert(
                header::CONTENT_RANGE,
                HeaderValue::from_str(&format!("bytes {}-{}/{length}", range.start, range.end))
                    .expect("valid header"),
            );
            response
        }
        Some(Ok(ranges)) => multipart_zeros(&ranges, length),
    };
    let headers = response.headers_mut();
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    headers.insert(
        header::LAST_MODIFIED,
        HeaderValue::from_str(&modified).expect("valid header"),
    );
    headers
        .entry(header::CONTENT_TYPE)
        .or_insert(HeaderValue::from_static("application/octet-stream"));
    Ok(response)
}

fn zero_stream(
    mut remaining: u64,
) -> impl futures_core::Stream<Item = Result<Bytes, std::io::Error>> {
    stream! {
        while remaining > 0 {
            let chunk = remaining.min(ZEROS.len() as u64);
            remaining -= chunk;
            yield Ok(Bytes::from_static(&ZEROS[..usize::try_from(chunk).expect("chunk fits")]));
        }
    }
}

fn zeros_response(status: StatusCode, length: u64) -> Response<Body> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_LENGTH, length)
        .body(Body::from_stream(zero_stream(length)))
        .expect("valid zeros response")
}

/// Go's multipart writer: a 60-character boundary and one part per range.
fn multipart_zeros(ranges: &[ByteRange], length: u64) -> Response<Body> {
    let boundary = format!("{:060x}", length);
    let parts: Vec<(String, u64)> = ranges
        .iter()
        .map(|range| {
            (
                format!(
                    "--{boundary}\r\nContent-Range: bytes {}-{}/{length}\r\nContent-Type: application/octet-stream\r\n\r\n",
                    range.start, range.end
                ),
                range.len(),
            )
        })
        .collect();
    let closing = format!("--{boundary}--\r\n");
    let content_length = parts
        .iter()
        .map(|(head, len)| head.len() as u64 + len + 2)
        .sum::<u64>()
        + closing.len() as u64;
    let body = stream! {
        for (head, len) in parts {
            yield Ok::<_, std::io::Error>(Bytes::from(head));
            for await chunk in zero_stream(len) {
                yield chunk;
            }
            yield Ok(Bytes::from_static(b"\r\n"));
        }
        yield Ok(Bytes::from(closing));
    };
    Response::builder()
        .status(StatusCode::PARTIAL_CONTENT)
        .header(header::CONTENT_LENGTH, content_length)
        .header(
            header::CONTENT_TYPE,
            format!("multipart/byteranges; boundary={boundary}"),
        )
        .body(Body::from_stream(body))
        .expect("valid multipart response")
}

/// MatriX.145 prints anacrolix's internal client dump here. Rustorr prints
/// the same per-torrent facts in its own words; no client parses this page.
pub(crate) async fn stat(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response<Body>, Response<Body>> {
    if !management_authorized(&state, &headers) {
        return Err(unauthorized());
    }
    let TorrentReply::List(torrents) = state
        .core
        .torrents(TorrentCommand::List)
        .await
        .map_err(|error| lifecycle(error).into_response())?
    else {
        return Err(ApiError::Status(StatusCode::INTERNAL_SERVER_ERROR).into_response());
    };
    let mut text = format!("# Torrents: {}\n", torrents.len());
    for torrent in &torrents {
        text.push_str(&torrent_status(torrent));
    }
    Ok(([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], text).into_response())
}

fn torrent_status(torrent: &TorrentView) -> String {
    let total = torrent.torrent_size.unwrap_or(0);
    let loaded = torrent.loaded_size.unwrap_or(0);
    let percent = if total == 0 {
        0.0
    } else {
        loaded as f64 * 100.0 / total as f64
    };
    format!(
        "\n{}\n{percent:.6}% of {total} bytes ({})\nInfohash: {}\n",
        torrent.name.as_deref().unwrap_or(&torrent.title),
        si_bytes(total),
        torrent.hash.as_deref().unwrap_or_default()
    )
}

/// go-humanize `Bytes`: SI units, one decimal below ten.
fn si_bytes(bytes: u64) -> String {
    if bytes < 10 {
        return format!("{bytes} B");
    }
    let units = ["B", "kB", "MB", "GB", "TB", "PB", "EB"];
    let exponent = ((bytes as f64).ln() / 1000_f64.ln()).floor() as usize;
    let value = (bytes as f64 / 1000_f64.powi(exponent as i32) * 10.0 + 0.5).floor() / 10.0;
    if value < 10.0 {
        format!("{value:.1} {}", units[exponent])
    } else {
        format!("{value:.0} {}", units[exponent])
    }
}

pub(crate) async fn shutdown(
    State(state): State<AppState>,
    reason: Option<Path<String>>,
    headers: HeaderMap,
) -> Result<Response<Body>, Response<Body>> {
    if !management_authorized(&state, &headers) {
        return Err(unauthorized());
    }
    // In read-only mode only a shutdown that names a reason is honoured.
    let reason = reason
        .map(|Path(reason)| reason.replace('/', ""))
        .unwrap_or_default();
    if state.http.read_only && reason.is_empty() {
        return Err(StatusCode::FORBIDDEN.into_response());
    }
    if let Some(requested) = &state.http.shutdown {
        requested.notify_one();
    }
    Ok(StatusCode::OK.into_response())
}

/// Until the R8 interface exists the root answers with a short notice; the
/// reference's bundled web UI is intentionally not carried over.
pub(crate) async fn root(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response<Body>, Response<Body>> {
    if !management_authorized(&state, &headers) {
        return Err(unauthorized());
    }
    Ok((
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        "<!doctype html><title>Rustorr</title><p>Rustorr is running. The web interface is not bundled yet.</p>",
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::{Router, body::to_bytes};
    use rustorr_lifecycle::ClientCore;
    use tower::ServiceExt;

    use axum::http::Method;

    use super::*;
    use crate::{HttpConfig, ServerInfo, app::tests::playback_app, router_with_core};

    async fn send(app: &Router, method: Method, uri: &str, range: Option<&str>) -> Response<Body> {
        let mut request = Request::builder().method(method).uri(uri);
        if let Some(range) = range {
            request = request.header(header::RANGE, range);
        }
        app.clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn magnets_list_saved_torrents_as_the_reference_html() {
        let (app, _, hash) = playback_app();
        let response = send(&app, Method::GET, "/magnets", None).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/html; charset=utf-8"
        );
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(
            body,
            format!(
                "<div><p><a href='magnet:?xt=urn:btih:{hash}&dn=video.mp4'>magnet:?xt=urn:btih:{hash}</a></p></div>"
            )
        );
    }

    #[tokio::test]
    async fn download_serves_zero_ranges_and_head_is_not_routed() {
        let (app, _, _) = playback_app();
        let response = send(&app, Method::GET, "/download/1", Some("bytes=0-15")).await;
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            response.headers()[header::CONTENT_RANGE],
            "bytes 0-15/1048576"
        );
        assert_eq!(response.headers()[header::ACCEPT_RANGES], "bytes");
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "application/octet-stream"
        );
        assert!(response.headers().contains_key(header::LAST_MODIFIED));
        assert_eq!(
            to_bytes(response.into_body(), usize::MAX).await.unwrap(),
            vec![0; 16]
        );

        let response = send(&app, Method::GET, "/download/1", Some("bytes=0-1,4-5")).await;
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        let length: usize = response.headers()[header::CONTENT_LENGTH]
            .to_str()
            .unwrap()
            .parse()
            .unwrap();
        let boundary = response.headers()[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .strip_prefix("multipart/byteranges; boundary=")
            .unwrap()
            .to_owned();
        assert_eq!(boundary.len(), 60);
        assert_eq!(
            to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap()
                .len(),
            length
        );

        let response = send(&app, Method::GET, "/download/1", Some("bytes=2000000-")).await;
        assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(response.headers()[header::CONTENT_RANGE], "bytes */1048576");

        for uri in [
            "/download/1",
            "/magnets",
            "/stat",
            "/",
            "/storage/settings",
            "/tmdb/settings",
            "/settings",
        ] {
            assert_eq!(
                send(&app, Method::HEAD, uri, None).await.status(),
                StatusCode::NOT_FOUND,
                "{uri}"
            );
        }
    }

    #[tokio::test]
    async fn web_routes_require_basic_auth_when_it_is_enabled() {
        let (_, core, _) = playback_app();
        let directory = tempfile::tempdir().unwrap();
        let accounts = directory.path().join("accs.db");
        std::fs::write(&accounts, br#"{"client":"secret"}"#).unwrap();
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
        for uri in [
            "/",
            "/magnets",
            "/stat",
            "/download/1",
            "/shutdown",
            "/shutdown/now",
        ] {
            assert_eq!(
                send(&app, Method::GET, uri, None).await.status(),
                StatusCode::UNAUTHORIZED,
                "{uri}"
            );
        }
    }

    #[test]
    fn magnet_links_match_anacrolix() {
        let magnet = MagnetView {
            hash: "d272ca49e3f32a0a08c0c0599a0a9daa6bf5cb7d".parse().unwrap(),
            name: "Медиа коллекция".into(),
            trackers: vec!["http://tracker:6969/announce".into(), "wss://a.b~c".into()],
        };
        assert_eq!(
            magnet_uri(&magnet),
            "magnet:?xt=urn:btih:d272ca49e3f32a0a08c0c0599a0a9daa6bf5cb7d\
             &dn=%D0%9C%D0%B5%D0%B4%D0%B8%D0%B0+%D0%BA%D0%BE%D0%BB%D0%BB%D0%B5%D0%BA%D1%86%D0%B8%D1%8F\
             &tr=http%3A%2F%2Ftracker%3A6969%2Fannounce&tr=wss%3A%2F%2Fa.b~c"
        );
        let bare = MagnetView {
            name: String::new(),
            trackers: Vec::new(),
            ..magnet
        };
        assert_eq!(
            magnet_uri(&bare),
            "magnet:?xt=urn:btih:d272ca49e3f32a0a08c0c0599a0a9daa6bf5cb7d&"
        );
    }

    #[test]
    fn sizes_are_humanised_like_go_humanize() {
        assert_eq!(si_bytes(9), "9 B");
        assert_eq!(si_bytes(622_592), "623 kB");
        assert_eq!(si_bytes(4_692_251_852), "4.7 GB");
        assert_eq!(si_bytes(8_388_608), "8.4 MB");
    }
}

#[cfg(test)]
mod process_mode_tests {
    use std::sync::Arc;

    use axum::{
        Router,
        body::to_bytes,
        http::{Method, header},
    };
    use rustorr_lifecycle::ClientCore;
    use tower::ServiceExt;

    use super::*;
    use crate::{HttpConfig, ServerInfo, app::tests::playback_app, router_with_core};

    fn app_with(config: HttpConfig) -> (Router, rustorr_lifecycle::InfoHash) {
        let (_, core, hash) = playback_app();
        let client: Arc<dyn ClientCore> = core;
        let app = router_with_core(
            ServerInfo {
                version: "MatriX.145".into(),
            },
            client,
            config,
        );
        (app, hash)
    }

    async fn send(
        app: &Router,
        method: Method,
        uri: &str,
        body: &str,
    ) -> (StatusCode, HeaderMap, String) {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_owned()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, headers, String::from_utf8_lossy(&body).into_owned())
    }

    #[tokio::test]
    async fn streams_over_the_size_limit_are_refused_like_go_http_error() {
        let (app, hash) = app_with(HttpConfig {
            max_stream_size: Some(9),
            ..HttpConfig::default()
        });
        for (method, uri) in [
            (Method::GET, format!("/play/{hash}/1")),
            (Method::HEAD, format!("/play/{hash}/1")),
            (Method::GET, format!("/stream?link={hash}&index=1&play")),
        ] {
            let (status, headers, body) = send(&app, method.clone(), &uri, "").await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}");
            assert_eq!(headers[header::CONTENT_TYPE], "text/plain; charset=utf-8");
            assert_eq!(headers["x-content-type-options"], "nosniff");
            if method == Method::GET {
                assert_eq!(body, "file size exceeded max allowed 9 bytes\n");
            }
        }
        let (app, hash) = app_with(HttpConfig {
            max_stream_size: Some(10),
            ..HttpConfig::default()
        });
        assert_eq!(
            send(&app, Method::GET, &format!("/play/{hash}/1"), "")
                .await
                .0,
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn read_only_mode_refuses_management_writes() {
        let (app, _) = app_with(HttpConfig {
            read_only: true,
            ..HttpConfig::default()
        });
        let refused = (
            StatusCode::FORBIDDEN,
            r#"{"error":"Read-only mode"}"#.to_owned(),
        );
        let (status, _, body) = send(
            &app,
            Method::POST,
            "/waf",
            r#"{"whitelist":"","blacklist":"","referers":""}"#,
        )
        .await;
        assert_eq!((status, body), refused);
        let (status, _, body) = send(
            &app,
            Method::POST,
            "/storage/settings",
            r#"{"settings":"json"}"#,
        )
        .await;
        assert_eq!((status, body), refused);
        let (_, _, body) = send(&app, Method::GET, "/waf", "").await;
        assert!(body.contains(r#""read_only":true"#), "{body}");
        let (status, _, body) = send(&app, Method::GET, "/shutdown", "").await;
        assert_eq!((status, body.as_str()), (StatusCode::FORBIDDEN, ""));
        assert_eq!(
            send(&app, Method::GET, "/shutdown/maintenance", "").await.0,
            StatusCode::OK
        );
    }
}
