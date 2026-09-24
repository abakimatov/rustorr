//! The web interface (R8), embedded at compile time by `build.rs`: `index.html`
//! at `/`, content-hashed files under `/assets/`, and the other files of the
//! build's root (manifest, icons) at their own paths. Everything is behind HTTP
//! authentication except `site.webmanifest`, as MatriX.145 serves its pages.

use axum::{
    Router,
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, Response, StatusCode, header},
    response::IntoResponse,
    routing::get,
};

use crate::{
    ApiError,
    app::{AppState, management_authorized, unauthorized},
    web_api,
};

mod embedded {
    include!(concat!(env!("OUT_DIR"), "/web_assets.rs"));
}

/// Served without authentication: the browser fetches it without
/// credentials when installing the app.
const PUBLIC: &[&str] = &["/site.webmanifest"];

fn file(path: &str) -> Option<&'static [u8]> {
    embedded::FILES
        .iter()
        .find(|(url, _)| *url == path)
        .map(|(_, bytes)| *bytes)
}

/// `/`, `/assets/*` and the build's root files, added to `router`.
pub(crate) fn routes(router: Router<AppState>) -> Router<AppState> {
    let mut router = router
        .route("/", get(index))
        .route("/assets/{*path}", get(asset));
    for (url, _) in embedded::FILES {
        let root_file = url.matches('/').count() == 1 && *url != "/index.html";
        if root_file {
            router = router.route(url, get(root_file_handler));
        }
    }
    router
}

fn content_type(path: &str) -> String {
    if path.ends_with(".webmanifest") {
        return "application/manifest+json".into();
    }
    let guessed = mime_guess::from_path(path).first_or_octet_stream();
    match guessed.type_() {
        mime_guess::mime::TEXT => format!("{guessed}; charset=utf-8"),
        _ if guessed.essence_str() == "application/javascript" => {
            "text/javascript; charset=utf-8".into()
        }
        _ => guessed.to_string(),
    }
}

fn respond(path: &str, bytes: &'static [u8], cache: &'static str) -> Response<Body> {
    let mut response = Response::new(Body::from(bytes));
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&content_type(path)).expect("a valid content type"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    response
}

async fn index(
    state: State<AppState>,
    headers: HeaderMap,
) -> Result<Response<Body>, Response<Body>> {
    match file("/index.html") {
        Some(bytes) => {
            if !management_authorized(&state, &headers) {
                return Err(unauthorized());
            }
            Ok(respond("/index.html", bytes, "no-cache"))
        }
        None => web_api::root(state, headers).await,
    }
}

async fn asset(
    State(state): State<AppState>,
    Path(path): Path<String>,
    headers: HeaderMap,
) -> Result<Response<Body>, Response<Body>> {
    if !management_authorized(&state, &headers) {
        return Err(unauthorized());
    }
    let url = format!("/assets/{path}");
    let bytes = file(&url).ok_or_else(|| ApiError::NotFound.into_response())?;
    // Names carry a content hash, so a file never changes under its name.
    Ok(respond(&url, bytes, "public, max-age=31536000, immutable"))
}

async fn root_file_handler(
    State(state): State<AppState>,
    uri: axum::http::Uri,
    headers: HeaderMap,
) -> Result<Response<Body>, Response<Body>> {
    let path = uri.path();
    if !PUBLIC.contains(&path) && !management_authorized(&state, &headers) {
        return Err(unauthorized());
    }
    let bytes = file(path).ok_or_else(|| StatusCode::NOT_FOUND.into_response())?;
    Ok(respond(path, bytes, "no-cache"))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::{body::to_bytes, http::Request};
    use rustorr_lifecycle::ClientCore;
    use tower::ServiceExt;

    use super::*;
    use crate::{HttpConfig, ServerInfo, app::tests::playback_app, router_with_core};

    fn app_with_auth() -> (Router, tempfile::TempDir) {
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
        (app, directory)
    }

    async fn get(app: &Router, path: &str, authorized: bool) -> (StatusCode, HeaderMap, Vec<u8>) {
        let mut request = Request::builder().uri(path);
        if authorized {
            // `client:secret`.
            request = request.header(header::AUTHORIZATION, "Basic Y2xpZW50OnNlY3JldA==");
        }
        let response = app
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, headers, body.to_vec())
    }

    #[tokio::test]
    async fn the_interface_is_served_behind_authentication() {
        let (app, _dir) = app_with_auth();
        assert_eq!(get(&app, "/", false).await.0, StatusCode::UNAUTHORIZED);
        let (status, headers, body) = get(&app, "/", true).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers[header::CONTENT_TYPE], "text/html; charset=utf-8");
        assert!(String::from_utf8_lossy(&body).contains("<title>"));
        assert_eq!(
            get(&app, "/assets/missing.js", true).await.0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            get(&app, "/assets/missing.js", false).await.0,
            StatusCode::UNAUTHORIZED
        );
        // Unknown paths keep the reference's 404.
        let (status, _, body) = get(&app, "/no/such/page", true).await;
        assert_eq!(
            (status, body.as_slice()),
            (StatusCode::NOT_FOUND, &b"404 page not found"[..])
        );
    }

    #[tokio::test]
    async fn built_files_are_served_with_their_cache_rules() {
        let (app, _dir) = app_with_auth();
        let Some((asset, bytes)) = embedded::FILES
            .iter()
            .find(|(url, _)| url.starts_with("/assets/"))
        else {
            // A build without the interface has nothing more to serve.
            assert!(embedded::FILES.is_empty());
            return;
        };
        let (status, headers, body) = get(&app, asset, true).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            headers[header::CACHE_CONTROL],
            "public, max-age=31536000, immutable"
        );
        assert_eq!(body, *bytes);
        assert_eq!(
            get(&app, "/", true).await.1[header::CACHE_CONTROL],
            "no-cache"
        );
        if file("/site.webmanifest").is_some() {
            let (status, headers, _) = get(&app, "/site.webmanifest", false).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(headers[header::CONTENT_TYPE], "application/manifest+json");
        }
        if file("/favicon.svg").is_some() {
            assert_eq!(
                get(&app, "/favicon.svg", false).await.0,
                StatusCode::UNAUTHORIZED
            );
            assert_eq!(get(&app, "/favicon.svg", true).await.0, StatusCode::OK);
        }
    }

    #[test]
    fn content_types_suit_browsers() {
        assert_eq!(
            content_type("/assets/index-1.js"),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(
            content_type("/assets/index-1.css"),
            "text/css; charset=utf-8"
        );
        assert_eq!(content_type("/index.html"), "text/html; charset=utf-8");
        assert_eq!(
            content_type("/site.webmanifest"),
            "application/manifest+json"
        );
        assert_eq!(content_type("/favicon.svg"), "image/svg+xml");
        assert_eq!(content_type("/assets/font.woff2"), "font/woff2");
    }
}
