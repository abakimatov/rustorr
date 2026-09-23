//! `/gst/settings`: MatriX.145's GStreamer settings. A build without the
//! `gst` tag answers `GET` with `{"built_in":false}` and `POST` with a JSON
//! `404`, and mounts none of the other `/gst/*` routes.

use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, Response, StatusCode},
    response::IntoResponse,
};

use crate::{
    ApiError,
    app::{AppState, json_response, management_authorized, unauthorized},
};

pub(crate) async fn get_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response<Body>, Response<Body>> {
    if !management_authorized(&state, &headers) {
        return Err(unauthorized());
    }
    json_response(serde_json::json!({ "built_in": false })).map_err(IntoResponse::into_response)
}

pub(crate) async fn set_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response<Body>, Response<Body>> {
    if !management_authorized(&state, &headers) {
        return Err(unauthorized());
    }
    Err(ApiError::Json {
        status: StatusCode::NOT_FOUND,
        message: "gstreamer is not built in".into(),
    }
    .into_response())
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
}
