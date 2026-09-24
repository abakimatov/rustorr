//! MatriX.145's `--weblogpath`: `log.WebLogger` writes one line per request
//! to its own file, in gin's words: status, client IP, method, the quoted
//! path with its query, and the request body.

use std::{
    fs::{File, OpenOptions},
    io::{self, Write},
    net::IpAddr,
    path::Path,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    body::{Body, to_bytes},
    extract::State,
    http::{HeaderMap, Request, header},
    middleware::Next,
    response::Response,
};

use crate::app::{AppState, peer};

/// The open web log.
pub struct AccessLog {
    file: Mutex<File>,
}

impl AccessLog {
    /// Opens `path` for appending, creating it if needed.
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            file: Mutex::new(file),
        })
    }

    /// `log.New(file, " ", log.LstdFlags).Println`: a space, the date and
    /// time, the text.
    fn write(&self, text: &str) {
        let line = format!(" {} {text}\n", timestamp(SystemTime::now()));
        let mut file = self.file.lock().unwrap_or_else(|error| error.into_inner());
        if let Err(error) = file.write_all(line.as_bytes()) {
            tracing::warn!(%error, "cannot write the web log");
        }
    }
}

/// `log.WebLogger`.
pub(crate) async fn middleware(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let Some(log) = state.http.access_log.clone() else {
        return next.run(request).await;
    };
    let method = request.method().to_string();
    // `URL.Path` is decoded, `RawQuery` is not.
    let mut path = percent_encoding::percent_decode_str(request.uri().path())
        .decode_utf8_lossy()
        .into_owned();
    if let Some(query) = request.uri().query().filter(|query| !query.is_empty()) {
        path.push('?');
        path.push_str(query);
    }
    let ip = client_ip(request.headers(), peer(&request));
    let multipart = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("multipart/form-data"));
    let (request, body) = if multipart {
        (request, "body hidden, too large".to_string())
    } else {
        let (parts, body) = request.into_parts();
        let bytes = to_bytes(body, usize::MAX).await.unwrap_or_default();
        let text = String::from_utf8_lossy(&bytes).into_owned();
        (Request::from_parts(parts, Body::from(bytes)), text)
    };
    let response = next.run(request).await;
    log.write(&format!(
        "{:>3} | {ip:>12} | {method:<7} {} {body}",
        response.status().as_u16(),
        go_quote(&path)
    ));
    response
}

/// gin's `ClientIP` with its defaults, which MatriX.145 keeps: every proxy is
/// trusted, so the first address of `X-Forwarded-For`, else `X-Real-IP`, else
/// the socket peer. A header with an unparsable address is skipped.
fn client_ip(headers: &HeaderMap, peer: IpAddr) -> String {
    for name in ["x-forwarded-for", "x-real-ip"] {
        let Some(value) = headers.get(name).and_then(|value| value.to_str().ok()) else {
            continue;
        };
        let items: Vec<&str> = value.split(',').map(str::trim).collect();
        if items.iter().all(|item| item.parse::<IpAddr>().is_ok()) {
            return items[0].to_string();
        }
    }
    peer.to_canonical().to_string()
}

/// `%#v` of a Go string: `strconv.Quote`.
fn go_quote(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('"');
    for char in value.chars() {
        match char {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            '\u{7}' => quoted.push_str("\\a"),
            '\u{8}' => quoted.push_str("\\b"),
            '\u{c}' => quoted.push_str("\\f"),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            '\u{b}' => quoted.push_str("\\v"),
            char if (char as u32) < 0x20 || char == '\u{7f}' => {
                quoted.push_str(&format!("\\x{:02x}", char as u32));
            }
            char if char.is_control() => {
                if (char as u32) > 0xffff {
                    quoted.push_str(&format!("\\U{:08x}", char as u32));
                } else {
                    quoted.push_str(&format!("\\u{:04x}", char as u32));
                }
            }
            char => quoted.push(char),
        }
    }
    quoted.push('"');
    quoted
}

/// `2006/01/02 15:04:05` in UTC. Go's logger uses the local zone, which is
/// UTC in the container images of both servers.
fn timestamp(time: SystemTime) -> String {
    let seconds = time
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let days = (seconds / 86_400) as i64;
    let rest = seconds % 86_400;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}/{month:02}/{day:02} {:02}:{:02}:{:02}",
        rest / 3600,
        rest / 60 % 60,
        rest % 60
    )
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use axum::http::HeaderValue;

    use super::*;

    #[test]
    fn timestamps_and_quoting_match_go() {
        assert_eq!(
            timestamp(UNIX_EPOCH + Duration::from_secs(1_790_190_123)),
            "2026/09/23 19:02:03"
        );
        assert_eq!(timestamp(UNIX_EPOCH), "1970/01/01 00:00:00");
        assert_eq!(
            timestamp(UNIX_EPOCH + Duration::from_secs(951_782_400)),
            "2000/02/29 00:00:00"
        );
        assert_eq!(go_quote("/echo?a=\"b\"\n"), r#""/echo?a=\"b\"\n""#);
        assert_eq!(go_quote("/файл\u{1}"), "\"/файл\\x01\"");
    }

    #[test]
    fn the_client_ip_follows_gins_defaults() {
        let peer: IpAddr = "10.0.0.9".parse().unwrap();
        let mut headers = HeaderMap::new();
        assert_eq!(client_ip(&headers, peer), "10.0.0.9");
        headers.insert("x-real-ip", HeaderValue::from_static("192.0.2.7"));
        assert_eq!(client_ip(&headers, peer), "192.0.2.7");
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("198.51.100.1, 10.0.0.2"),
        );
        assert_eq!(client_ip(&headers, peer), "198.51.100.1");
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("junk, 10.0.0.2"),
        );
        assert_eq!(client_ip(&headers, peer), "192.0.2.7");
        assert_eq!(
            client_ip(&HeaderMap::new(), "::ffff:10.0.0.9".parse().unwrap()),
            "10.0.0.9"
        );
    }
}
