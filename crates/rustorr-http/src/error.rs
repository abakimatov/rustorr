use axum::{
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};

/// Error responses in the shapes the reference produces, taken from the R2
/// corpus. This is the only place an error becomes an HTTP response; a route
/// picks the shape the reference uses for that route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiError {
    /// `404` with the plain-text body the reference gives to every unknown
    /// path and to every known path requested with the wrong method.
    NotFound,
    /// A JSON body `{"error": message}`.
    Json { status: StatusCode, message: String },
    /// A status and no body, which the reference does for some `400`s.
    Status(StatusCode),
}

/// JSON as Go's `encoding/json` writes it, which gin's `c.JSON` uses: `<`,
/// `>`, `&`, U+2028 and U+2029 are escaped. They can only occur inside
/// strings, so escaping the serialised bytes is safe.
pub(crate) fn go_json(value: &impl serde::Serialize) -> serde_json::Result<Vec<u8>> {
    let text = serde_json::to_string(value)?;
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '<' => escaped.push_str("\\u003c"),
            '>' => escaped.push_str("\\u003e"),
            '&' => escaped.push_str("\\u0026"),
            '\u{2028}' => escaped.push_str("\\u2028"),
            '\u{2029}' => escaped.push_str("\\u2029"),
            other => escaped.push(other),
        }
    }
    Ok(escaped.into_bytes())
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        match self {
            Self::NotFound => (
                StatusCode::NOT_FOUND,
                [(header::CONTENT_TYPE, "text/plain")],
                "404 page not found",
            )
                .into_response(),
            Self::Json { status, message } => (
                status,
                [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
                go_json(&serde_json::json!({ "error": message }))
                    .expect("an error message serialises"),
            )
                .into_response(),
            Self::Status(status) => status.into_response(),
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::body::to_bytes;

    use super::*;

    async fn parts(error: ApiError) -> (StatusCode, Option<String>, Vec<u8>) {
        let response = error.into_response();
        let status = response.status();
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .map(|value| value.to_str().unwrap().to_owned());
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, content_type, body.to_vec())
    }

    #[test]
    fn json_is_html_escaped_like_go() {
        assert_eq!(
            go_json(&serde_json::json!({"magnet": "a&b<c>\u{2028}"})).unwrap(),
            br#"{"magnet":"a\u0026b\u003cc\u003e\u2028"}"#
        );
    }

    #[tokio::test]
    async fn not_found_is_the_reference_text_without_a_charset() {
        let (status, content_type, body) = parts(ApiError::NotFound).await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(content_type.as_deref(), Some("text/plain"));
        assert_eq!(body, b"404 page not found");
    }

    #[tokio::test]
    async fn a_json_error_matches_the_reference_body_byte_for_byte() {
        let (status, content_type, body) = parts(ApiError::Json {
            status: StatusCode::BAD_REQUEST,
            message: "unexpected EOF".into(),
        })
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            content_type.as_deref(),
            Some("application/json; charset=utf-8")
        );
        assert_eq!(body, br#"{"error":"unexpected EOF"}"#);
    }

    #[tokio::test]
    async fn a_json_error_message_is_escaped() {
        let message = "bad \"link\"\nline two — ошибка";

        let (_, _, body) = parts(ApiError::Json {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        })
        .await;

        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["error"], message);
    }

    #[tokio::test]
    async fn a_status_only_error_has_no_body_and_no_content_type() {
        let (status, content_type, body) = parts(ApiError::Status(StatusCode::BAD_REQUEST)).await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(content_type, None);
        assert!(body.is_empty());
    }
}
