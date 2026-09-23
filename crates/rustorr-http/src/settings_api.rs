//! Settings routes beside `/settings`: the storage choice and TMDB settings.

use std::collections::HashSet;

use axum::{
    body::{Body, to_bytes},
    extract::State,
    http::{HeaderMap, Request, Response, header},
    response::IntoResponse,
};
use rustorr_lifecycle::{SettingsCommand, ViewedCommand};
use serde_json::{Map, Value, json};

use crate::app::{
    AppState, json_bad_request, json_body, json_response, lifecycle, management_authorized,
    read_only_refused, unauthorized,
};

fn storage_name(json: bool) -> &'static str {
    if json { "json" } else { "bbolt" }
}

pub(crate) async fn get_storage(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response<Body>, Response<Body>> {
    if !management_authorized(&state, &headers) {
        return Err(unauthorized());
    }
    let settings = state
        .core
        .settings(SettingsCommand::Get)
        .await
        .map_err(|error| lifecycle(error).into_response())?;
    // MatriX.145 counts the top-level entries of its viewed store: one per
    // torrent, however many of its files were watched.
    let viewed = state
        .core
        .viewed(ViewedCommand::List { hash: None })
        .await
        .map_err(|error| lifecycle(error).into_response())?;
    let torrents: HashSet<_> = viewed.iter().map(|file| file.hash).collect();
    json_response(json!({
        "settings": storage_name(settings.store_settings_in_json),
        "viewed": storage_name(settings.store_viewed_in_json),
        "viewedCount": torrents.len(),
    }))
    .map_err(IntoResponse::into_response)
}

pub(crate) async fn set_storage(
    State(state): State<AppState>,
    request: Request<Body>,
) -> Result<Response<Body>, Response<Body>> {
    if !management_authorized(&state, request.headers()) {
        return Err(unauthorized());
    }
    if state.http.read_only {
        return Err(read_only_refused());
    }
    let form = request
        .headers()
        .get(header::CONTENT_TYPE)
        .is_some_and(|value| value == "application/x-www-form-urlencoded");
    let preferences = if form {
        form_preferences(request).await
    } else {
        match json_body::<Option<Map<String, Value>>>(request).await {
            Ok(preferences) => preferences.unwrap_or_default(),
            Err(error) => return Err(error.into_response()),
        }
    };
    // Like the reference, only string values are storage choices; any other
    // value still counts as "provided".
    let choice = |key: &str| {
        preferences
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
    };
    for (key, message) in [
        ("settings", "Invalid settings storage value"),
        ("viewed", "Invalid viewed storage value"),
    ] {
        if choice(key).is_some_and(|value| value != "json" && value != "bbolt") {
            return Err(json_bad_request(message).into_response());
        }
    }
    if preferences.is_empty() {
        return Err(json_bad_request("No preferences provided").into_response());
    }
    state
        .core
        .settings(SettingsCommand::SetStorage {
            settings_in_json: choice("settings").map(|value| value == "json"),
            viewed_in_json: choice("viewed").map(|value| value == "json"),
        })
        .await
        .map_err(|error| lifecycle(error).into_response())?;
    json_response(json!({"status": "ok"})).map_err(IntoResponse::into_response)
}

/// gin `PostForm`: an absent or empty field is not a preference.
async fn form_preferences(request: Request<Body>) -> Map<String, Value> {
    let body = to_bytes(request.into_body(), 1 << 20)
        .await
        .unwrap_or_default();
    let mut preferences = Map::new();
    for pair in body.split(|byte| *byte == b'&') {
        let text = String::from_utf8_lossy(pair);
        let (key, value) = text.split_once('=').unwrap_or((&text, ""));
        let value = form_decode(value);
        if (key == "settings" || key == "viewed")
            && !value.is_empty()
            && !preferences.contains_key(key)
        {
            preferences.insert(key.to_owned(), Value::String(value));
        }
    }
    preferences
}

fn form_decode(value: &str) -> String {
    percent_encoding::percent_decode_str(&value.replace('+', " "))
        .decode_utf8_lossy()
        .into_owned()
}

pub(crate) async fn tmdb(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response<Body>, Response<Body>> {
    if !management_authorized(&state, &headers) {
        return Err(unauthorized());
    }
    let settings = state
        .core
        .settings(SettingsCommand::Get)
        .await
        .map_err(|error| lifecycle(error).into_response())?;
    json_response(settings.tmdb_settings).map_err(IntoResponse::into_response)
}

#[cfg(test)]
mod tests {
    use axum::{
        Router,
        body::to_bytes,
        http::{Method, StatusCode},
    };
    use rustorr_lifecycle::ClientCore;
    use tower::ServiceExt;

    use super::*;
    use crate::app::tests::playback_app;

    async fn call(
        app: &Router,
        method: Method,
        uri: &str,
        content_type: &str,
        body: &str,
    ) -> (StatusCode, String) {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header(header::CONTENT_TYPE, content_type)
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
    async fn storage_preferences_round_trip_and_validate_like_the_reference() {
        let (app, core, hash) = playback_app();
        for index in [1, 2] {
            core.viewed(ViewedCommand::Set {
                hash,
                index,
                timecode: 0.0,
            })
            .await
            .unwrap();
        }
        let json = "application/json";
        assert_eq!(
            call(&app, Method::GET, "/storage/settings", json, "").await,
            (
                StatusCode::OK,
                r#"{"settings":"json","viewed":"bbolt","viewedCount":1}"#.into()
            )
        );
        assert_eq!(
            call(
                &app,
                Method::POST,
                "/storage/settings",
                json,
                r#"{"settings":"bbolt","viewed":"json"}"#
            )
            .await,
            (StatusCode::OK, r#"{"status":"ok"}"#.into())
        );
        assert_eq!(
            call(&app, Method::GET, "/storage/settings", json, "")
                .await
                .1,
            r#"{"settings":"bbolt","viewed":"json","viewedCount":1}"#
        );
        assert_eq!(
            call(
                &app,
                Method::POST,
                "/storage/settings",
                "application/x-www-form-urlencoded",
                "settings=json"
            )
            .await,
            (StatusCode::OK, r#"{"status":"ok"}"#.into())
        );
        let settings = core.settings(SettingsCommand::Get).await.unwrap();
        assert!(settings.store_settings_in_json);
        assert!(settings.store_viewed_in_json);
        for (body, error) in [
            (r#"{"settings":"sqlite"}"#, "Invalid settings storage value"),
            (r#"{"viewed":"yaml"}"#, "Invalid viewed storage value"),
            ("{}", "No preferences provided"),
            ("null", "No preferences provided"),
        ] {
            assert_eq!(
                call(&app, Method::POST, "/storage/settings", json, body).await,
                (StatusCode::BAD_REQUEST, format!(r#"{{"error":"{error}"}}"#)),
                "{body}"
            );
        }
    }

    #[tokio::test]
    async fn tmdb_settings_are_the_settings_document_section() {
        let (app, _, _) = playback_app();
        assert_eq!(
            call(&app, Method::GET, "/tmdb/settings", "application/json", "").await,
            (
                StatusCode::OK,
                r#"{"APIKey":"","APIURL":"https://api.themoviedb.org","ImageURL":"https://image.tmdb.org","ImageURLRu":"https://imagetmdb.com"}"#.into()
            )
        );
    }
}
