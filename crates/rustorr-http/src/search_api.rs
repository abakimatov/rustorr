//! Rutor and Torznab search routes.

use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, Request, Response, StatusCode},
    response::IntoResponse,
};
use rustorr_lifecycle::{Settings, SettingsCommand};
use rustorr_search::{CategoryType, Indexer, Search, SearchFuture, TorrentDetails};
use serde::Deserialize;
use serde_json::json;

use crate::{
    ApiError,
    app::{
        AppState, json_body, json_response, lifecycle, management_authorized, query, unauthorized,
    },
};

/// Stands in when no search integration is wired, as in router tests.
pub(crate) struct NoSearch;

impl Search for NoSearch {
    fn set_rutor_enabled(&self, _enabled: bool) -> SearchFuture<'_, ()> {
        Box::pin(async {})
    }

    fn rutor(&self, _query: &str) -> Vec<TorrentDetails> {
        Vec::new()
    }

    fn torznab<'a>(
        &'a self,
        _indexers: &'a [Indexer],
        _query: &'a str,
        _index: i64,
    ) -> SearchFuture<'a, Vec<TorrentDetails>> {
        Box::pin(async { Vec::new() })
    }

    fn torznab_test<'a>(
        &'a self,
        _host: &'a str,
        _key: &'a str,
    ) -> SearchFuture<'a, Result<(), String>> {
        Box::pin(async { Err("search is not available".into()) })
    }
}

fn search_authorized(state: &AppState, headers: &HeaderMap) -> bool {
    state.http.search_without_auth || management_authorized(state, headers)
}

async fn current_settings(state: &AppState) -> Result<Settings, Response<Body>> {
    state
        .core
        .settings(SettingsCommand::Get)
        .await
        .map_err(|error| lifecycle(error).into_response())
}

/// gin decodes `query` once and the handler runs `url.QueryUnescape` again,
/// keeping an empty string when that second pass fails.
fn search_query(request: &Request<Body>) -> String {
    let once = query(request.uri()).remove("query").unwrap_or_default();
    query_unescape(&once).unwrap_or_default()
}

fn query_unescape(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                let hex = text.get(i + 1..i + 3)?;
                decoded.push(u8::from_str_radix(hex, 16).ok()?);
                i += 3;
            }
            b'+' => {
                decoded.push(b' ');
                i += 1;
            }
            byte => {
                decoded.push(byte);
                i += 1;
            }
        }
    }
    Some(String::from_utf8_lossy(&decoded).into_owned())
}

fn disabled() -> Response<Body> {
    let mut response = json_response(json!([])).unwrap_or_else(IntoResponse::into_response);
    *response.status_mut() = StatusCode::BAD_REQUEST;
    response
}

pub(crate) async fn rutor(
    State(state): State<AppState>,
    request: Request<Body>,
) -> Result<Response<Body>, Response<Body>> {
    if !search_authorized(&state, request.headers()) {
        return Err(unauthorized());
    }
    if !current_settings(&state).await?.enable_rutor_search {
        return Err(disabled());
    }
    let found = state.search.rutor(&search_query(&request));
    json_response(found).map_err(IntoResponse::into_response)
}

pub(crate) async fn torznab(
    State(state): State<AppState>,
    request: Request<Body>,
) -> Result<Response<Body>, Response<Body>> {
    if !search_authorized(&state, request.headers()) {
        return Err(unauthorized());
    }
    let settings = current_settings(&state).await?;
    if !settings.enable_torznab_search {
        return Err(disabled());
    }
    let index = query(request.uri())
        .get("index")
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(-1);
    let indexers: Vec<Indexer> = settings
        .torznab_urls
        .unwrap_or_default()
        .into_iter()
        .map(|config| Indexer {
            host: config.host,
            key: config.key,
            categories: config.categories,
            category_type: CategoryType::parse(&config.category_type),
        })
        .collect();
    let found = state
        .search
        .torznab(&indexers, &search_query(&request), index)
        .await;
    json_response(found).map_err(IntoResponse::into_response)
}

#[derive(Deserialize)]
struct TestRequest {
    #[serde(default)]
    host: String,
    #[serde(default)]
    key: String,
}

pub(crate) async fn torznab_test(
    State(state): State<AppState>,
    request: Request<Body>,
) -> Result<Response<Body>, Response<Body>> {
    if !management_authorized(&state, request.headers()) {
        return Err(unauthorized());
    }
    // gin's AbortWithError: the status alone.
    let request: TestRequest = json_body(request)
        .await
        .map_err(|_| ApiError::Status(StatusCode::BAD_REQUEST).into_response())?;
    let body = match state.search.torznab_test(&request.host, &request.key).await {
        Ok(()) => json!({"success": true}),
        Err(error) => json!({"error": error, "success": false}),
    };
    json_response(body).map_err(IntoResponse::into_response)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::to_bytes;
    use rustorr_lifecycle::ClientCore;
    use tower::ServiceExt;

    use super::*;
    use crate::{HttpConfig, ServerInfo, app::tests::playback_app, router_with_core};

    #[tokio::test]
    async fn every_search_path_form_reaches_the_handler() {
        let (app, _, _) = playback_app();
        for uri in [
            "/search?query=x",
            "/search/?query=x",
            "/search/anything?query=x",
            "/torznab/search?query=x",
            "/torznab/search/?query=x",
            "/torznab/search/anything?query=x",
        ] {
            let response = app
                .clone()
                .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{uri}");
            assert_eq!(
                to_bytes(response.into_body(), usize::MAX).await.unwrap(),
                "[]"
            );
        }
    }

    #[tokio::test]
    async fn search_can_skip_authentication() {
        let (_, core, _) = playback_app();
        let directory = tempfile::tempdir().unwrap();
        let accounts = directory.path().join("accs.db");
        std::fs::write(&accounts, br#"{"client":"secret"}"#).unwrap();
        for (without_auth, expected) in [
            (false, StatusCode::UNAUTHORIZED),
            (true, StatusCode::BAD_REQUEST),
        ] {
            let client: Arc<dyn ClientCore> = core.clone();
            let app = router_with_core(
                ServerInfo {
                    version: "MatriX.145".into(),
                },
                client,
                HttpConfig {
                    credentials: Some(crate::Credentials::read(&accounts).unwrap()),
                    search_without_auth: without_auth,
                    ..HttpConfig::default()
                },
            );
            let response = app
                .oneshot(
                    Request::builder()
                        .uri("/search/?query=x")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), expected);
        }
    }

    #[test]
    fn the_second_unescape_matches_go() {
        assert_eq!(query_unescape("a+b%20c").as_deref(), Some("a b c"));
        assert_eq!(query_unescape("100%"), None);
        assert_eq!(query_unescape("%zz"), None);
        assert_eq!(query_unescape("фильм").as_deref(), Some("фильм"));
    }
}
