//! Go's `http.ServeContent` over bytes in memory, a local file or a torrent
//! file: conditional requests with ETags and dates, single and multipart
//! ranges, and the content type by name or by sniffing.

use std::{
    io::{self, SeekFrom},
    path::PathBuf,
    pin::Pin,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use async_stream::try_stream;
use axum::{
    body::{Body, Bytes},
    http::{HeaderMap, HeaderName, HeaderValue, Method, Response, StatusCode, header},
    response::IntoResponse,
};
use futures_core::Stream;
use rustorr_lifecycle::{ClientCore, InfoHash, PlaybackRequest};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;

use crate::{
    app::go_http_error,
    media_type,
    range::{self, ByteRange, RangeError},
};

/// Go's `DetectContentType` looks at no more than this many bytes.
const SNIFF_LEN: u64 = 512;

type ByteStream = Pin<Box<dyn Stream<Item = io::Result<Bytes>> + Send>>;

/// Where the content comes from.
#[derive(Clone)]
pub(crate) enum Source {
    Memory(Bytes),
    File(PathBuf),
    Torrent {
        core: Arc<dyn ClientCore>,
        hash: InfoHash,
        index: u32,
    },
}

impl Source {
    async fn reader(&self, start: u64, length: u64) -> io::Result<Pin<Box<dyn AsyncRead + Send>>> {
        match self {
            Self::Memory(bytes) => {
                let start = usize::try_from(start).map_err(io::Error::other)?;
                let end = start + usize::try_from(length).map_err(io::Error::other)?;
                let slice = bytes.slice(start.min(bytes.len())..end.min(bytes.len()));
                Ok(Box::pin(io::Cursor::new(slice)))
            }
            Self::File(path) => {
                let mut file = tokio::fs::File::open(path).await?;
                file.seek(SeekFrom::Start(start)).await?;
                Ok(Box::pin(file.take(length)))
            }
            Self::Torrent { core, hash, index } => {
                let end = (start + length).checked_sub(1);
                let playback = Arc::clone(core)
                    .playback(PlaybackRequest {
                        hash: *hash,
                        index: *index,
                        offset: start,
                        end,
                        prefetch_offset: None,
                    })
                    .await
                    .map_err(io::Error::other)?;
                Ok(Box::pin(playback.reader.take(length)))
            }
        }
    }

    async fn stream(self, start: u64, length: u64) -> io::Result<ByteStream> {
        let reader = self.reader(start, length).await?;
        Ok(Box::pin(ReaderStream::new(reader)))
    }

    async fn head(&self, size: u64) -> io::Result<Vec<u8>> {
        let mut reader = self.reader(0, size.min(SNIFF_LEN)).await?;
        let mut buffer = Vec::new();
        reader.read_to_end(&mut buffer).await?;
        Ok(buffer)
    }
}

/// What `ServeContent` is asked to serve.
pub(crate) struct Content<'a> {
    pub source: Source,
    pub size: u64,
    /// The name whose extension picks the type, as `ServeContent`'s `name`.
    pub name: &'a str,
    /// A type set by the handler before `ServeContent`, which then neither
    /// guesses nor sniffs.
    pub content_type: Option<&'a str>,
    /// Modification time in Unix seconds; `None` is Go's zero time.
    pub modified: Option<u64>,
    /// An `ETag` the handler set before `ServeContent`.
    pub etag: Option<&'a str>,
}

#[derive(Clone, Copy)]
enum Condition {
    None,
    True,
    False,
}

fn header_text(headers: &HeaderMap, name: header::HeaderName) -> &str {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
}

fn http_date(headers: &HeaderMap, name: header::HeaderName) -> Option<u64> {
    httpdate::parse_http_date(header_text(headers, name))
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|since| since.as_secs())
}

/// `scanETag`: the first entity tag and the rest.
fn scan_etag(text: &str) -> Option<(&str, &str)> {
    let text = text.trim_matches([' ', '\t', '\n', '\r']);
    let start = if text.starts_with("W/") { 2 } else { 0 };
    let bytes = text.as_bytes();
    if bytes.len() < start + 2 || bytes[start] != b'"' {
        return None;
    }
    for (index, &byte) in bytes.iter().enumerate().skip(start + 1) {
        match byte {
            0x21 | 0x23..=0x7e | 0x80.. => {}
            b'"' => return Some((&text[..=index], &text[index + 1..])),
            _ => return None,
        }
    }
    None
}

fn strong_match(tag: &str, etag: Option<&str>) -> bool {
    etag.is_some_and(|etag| tag == etag && tag.starts_with('"'))
}

fn weak_match(tag: &str, etag: Option<&str>) -> bool {
    tag.trim_start_matches("W/") == etag.unwrap_or_default().trim_start_matches("W/")
}

/// Walks a comma-separated entity-tag list; `*` answers `star`.
fn tag_list(list: &str, star: Condition, mut matches: impl FnMut(&str) -> bool) -> Condition {
    let mut rest = list;
    loop {
        rest = rest.trim_matches([' ', '\t', '\n', '\r']);
        if rest.is_empty() {
            break;
        }
        if let Some(after) = rest.strip_prefix(',') {
            rest = after;
            continue;
        }
        if rest.starts_with('*') {
            return star;
        }
        let Some((tag, remaining)) = scan_etag(rest) else {
            break;
        };
        if matches(tag) {
            return match star {
                Condition::True => Condition::True,
                _ => Condition::False,
            };
        }
        rest = remaining;
    }
    match star {
        Condition::True => Condition::False,
        _ => Condition::True,
    }
}

enum Outcome {
    PreconditionFailed,
    NotModified,
    Proceed(Option<String>),
}

/// `checkPreconditions`.
fn preconditions(method: &Method, headers: &HeaderMap, content: &Content<'_>) -> Outcome {
    let get_or_head = matches!(*method, Method::GET | Method::HEAD);
    let if_match = header_text(headers, header::IF_MATCH);
    let mut check = if if_match.is_empty() {
        Condition::None
    } else {
        tag_list(if_match, Condition::True, |tag| {
            strong_match(tag, content.etag)
        })
    };
    if matches!(check, Condition::None) {
        check = match (
            content.modified,
            http_date(headers, header::IF_UNMODIFIED_SINCE),
        ) {
            (Some(modified), Some(since)) if modified <= since => Condition::True,
            (Some(_), Some(_)) => Condition::False,
            _ => Condition::None,
        };
    }
    if matches!(check, Condition::False) {
        return Outcome::PreconditionFailed;
    }
    let if_none_match = header_text(headers, header::IF_NONE_MATCH);
    let none_match = if if_none_match.is_empty() {
        Condition::None
    } else {
        tag_list(if_none_match, Condition::False, |tag| {
            weak_match(tag, content.etag)
        })
    };
    match none_match {
        Condition::False => {
            return if get_or_head {
                Outcome::NotModified
            } else {
                Outcome::PreconditionFailed
            };
        }
        Condition::None => {
            if get_or_head
                && let (Some(modified), Some(since)) = (
                    content.modified,
                    http_date(headers, header::IF_MODIFIED_SINCE),
                )
                && modified <= since
            {
                return Outcome::NotModified;
            }
        }
        Condition::True => {}
    }
    let range = header_text(headers, header::RANGE);
    if range.is_empty() {
        return Outcome::Proceed(None);
    }
    let if_range = header_text(headers, header::IF_RANGE);
    if get_or_head && !if_range.is_empty() {
        let fresh = match scan_etag(if_range) {
            Some((tag, _)) => strong_match(tag, content.etag),
            None => {
                content.modified.is_some()
                    && http_date(headers, header::IF_RANGE) == content.modified
            }
        };
        if !fresh {
            return Outcome::Proceed(None);
        }
    }
    Outcome::Proceed(Some(range.to_owned()))
}

fn set(headers: &mut HeaderMap, name: HeaderName, value: &str) {
    if let Ok(value) = HeaderValue::from_str(value) {
        headers.insert(name, value);
    }
}

fn set_last_modified(headers: &mut HeaderMap, modified: Option<u64>) {
    if let Some(modified) = modified {
        set(
            headers,
            header::LAST_MODIFIED,
            &httpdate::fmt_http_date(UNIX_EPOCH + Duration::from_secs(modified)),
        );
    }
}

/// Headers the handler set before `ServeContent`, which every answer keeps.
fn base_headers(content: &Content<'_>) -> HeaderMap {
    let mut headers = HeaderMap::new();
    if let Some(etag) = content.etag {
        set(&mut headers, header::ETAG, etag);
    }
    set_last_modified(&mut headers, content.modified);
    headers
}

fn with_headers(mut response: Response<Body>, headers: HeaderMap) -> Response<Body> {
    for (name, value) in &headers {
        response.headers_mut().insert(name.clone(), value.clone());
    }
    response
}

/// `serveError`, which drops the cache validators.
fn serve_error(status: StatusCode, message: &str, content: &Content<'_>) -> Response<Body> {
    let mut headers = base_headers(content);
    headers.remove(header::ETAG);
    headers.remove(header::LAST_MODIFIED);
    with_headers(go_http_error(status, message), headers)
}

pub(crate) async fn serve(
    content: Content<'_>,
    method: &Method,
    headers: &HeaderMap,
) -> Response<Body> {
    let range_header = match preconditions(method, headers, &content) {
        Outcome::PreconditionFailed => {
            return with_headers(
                StatusCode::PRECONDITION_FAILED.into_response(),
                base_headers(&content),
            );
        }
        Outcome::NotModified => {
            // `writeNotModified`: Last-Modified goes when an ETag is present.
            let mut kept = base_headers(&content);
            if content.etag.is_some() {
                kept.remove(header::LAST_MODIFIED);
            }
            return with_headers(StatusCode::NOT_MODIFIED.into_response(), kept);
        }
        Outcome::Proceed(range) => range,
    };
    let content_type = match content.content_type {
        Some(content_type) => content_type.to_owned(),
        None => match media_type::by_extension(content.name) {
            Some(content_type) => content_type,
            None => match content.source.head(content.size).await {
                Ok(head) => sniff(&head).to_owned(),
                Err(_) => {
                    return serve_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "seeker can't seek",
                        &content,
                    );
                }
            },
        },
    };
    let size = content.size;
    let ranges = match range_header.map(|value| range::parse(&value, size)) {
        None => Vec::new(),
        Some(Ok(ranges)) if ranges.iter().map(|range| range.len()).sum::<u64>() > size => {
            Vec::new()
        }
        Some(Ok(ranges)) => ranges,
        Some(Err(RangeError::Unsatisfiable)) if size == 0 => Vec::new(),
        Some(Err(RangeError::Unsatisfiable)) => {
            let mut response = serve_error(
                StatusCode::RANGE_NOT_SATISFIABLE,
                "invalid range: failed to overlap",
                &content,
            );
            set(
                response.headers_mut(),
                header::CONTENT_RANGE,
                &format!("bytes */{size}"),
            );
            return response;
        }
        Some(Err(RangeError::Invalid)) => {
            return serve_error(StatusCode::RANGE_NOT_SATISFIABLE, "invalid range", &content);
        }
    };
    let head = *method == Method::HEAD;
    let body = |stream: io::Result<ByteStream>| match stream {
        Ok(stream) => Ok(Body::from_stream(stream)),
        Err(error) => Err(error),
    };
    let mut response = match ranges.as_slice() {
        [] => {
            let body = if head {
                Ok(Body::empty())
            } else {
                body(content.source.clone().stream(0, size).await)
            };
            let Ok(body) = body else {
                return serve_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "cannot read content",
                    &content,
                );
            };
            let mut response = Response::new(body);
            set(response.headers_mut(), header::CONTENT_TYPE, &content_type);
            response
                .headers_mut()
                .insert(header::CONTENT_LENGTH, size.into());
            response
        }
        [range] => {
            let body = if head {
                Ok(Body::empty())
            } else {
                body(
                    content
                        .source
                        .clone()
                        .stream(range.start, range.len())
                        .await,
                )
            };
            let Ok(body) = body else {
                return serve_error(StatusCode::RANGE_NOT_SATISFIABLE, "cannot seek", &content);
            };
            let mut response = Response::new(body);
            *response.status_mut() = StatusCode::PARTIAL_CONTENT;
            let headers = response.headers_mut();
            set(headers, header::CONTENT_TYPE, &content_type);
            headers.insert(header::CONTENT_LENGTH, range.len().into());
            set(
                headers,
                header::CONTENT_RANGE,
                &format!("bytes {}-{}/{size}", range.start, range.end),
            );
            response
        }
        ranges => multipart(
            content.source.clone(),
            ranges.to_vec(),
            size,
            &content_type,
            head,
        ),
    };
    let headers = response.headers_mut();
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    for (name, value) in &base_headers(&content) {
        headers.insert(name.clone(), value.clone());
    }
    response
}

/// Go draws 30 random bytes; only the 60-character length is observable.
fn boundary() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    format!("{nanos:060x}")
}

/// `multipart/byteranges` as Go's multipart writer frames it.
fn multipart(
    source: Source,
    ranges: Vec<ByteRange>,
    size: u64,
    content_type: &str,
    head: bool,
) -> Response<Body> {
    let boundary = boundary();
    let heads: Vec<String> = ranges
        .iter()
        .map(|range| {
            format!(
                "--{boundary}\r\nContent-Range: bytes {}-{}/{size}\r\nContent-Type: {content_type}\r\n\r\n",
                range.start, range.end
            )
        })
        .collect();
    let closing = format!("--{boundary}--\r\n");
    let length = heads
        .iter()
        .zip(&ranges)
        .map(|(head, range)| head.len() as u64 + range.len() + 2)
        .sum::<u64>()
        + closing.len() as u64;
    let body = if head {
        Body::empty()
    } else {
        let stream: ByteStream = Box::pin(try_stream! {
            for (part, range) in heads.into_iter().zip(ranges) {
                yield Bytes::from(part);
                let mut reader = source.reader(range.start, range.len()).await?;
                let mut buffer = vec![0; 32 * 1024];
                loop {
                    let count = reader.read(&mut buffer).await?;
                    if count == 0 {
                        break;
                    }
                    yield Bytes::copy_from_slice(&buffer[..count]);
                }
                yield Bytes::from_static(b"\r\n");
            }
            yield Bytes::from(closing);
        });
        Body::from_stream(stream)
    };
    Response::builder()
        .status(StatusCode::PARTIAL_CONTENT)
        .header(
            header::CONTENT_TYPE,
            format!("multipart/byteranges; boundary={boundary}"),
        )
        .header(header::CONTENT_LENGTH, length)
        .body(body)
        .expect("valid multipart response")
}

/// `DetectContentType` of a source's first bytes.
pub(crate) async fn sniff_source(source: &Source, size: u64) -> String {
    source
        .head(size)
        .await
        .map_or_else(|_| sniff(&[]).to_owned(), |head| sniff(&head).to_owned())
}

/// A subset of Go's `DetectContentType`: common binary signatures, then the
/// text-or-binary decision on control bytes.
pub(crate) fn sniff(data: &[u8]) -> &'static str {
    const SIGNATURES: &[(&[u8], &str)] = &[
        (b"%PDF-", "application/pdf"),
        (b"\x89PNG\r\n\x1a\n", "image/png"),
        (b"\xff\xd8\xff", "image/jpeg"),
        (b"GIF87a", "image/gif"),
        (b"GIF89a", "image/gif"),
        (b"\x1a\x45\xdf\xa3", "video/webm"),
        (b"ID3", "audio/mpeg"),
        (b"OggS\x00", "application/ogg"),
        (b"PK\x03\x04", "application/zip"),
        (b"\x1f\x8b\x08", "application/x-gzip"),
    ];
    if let Some((_, content_type)) = SIGNATURES
        .iter()
        .find(|(signature, _)| data.starts_with(signature))
    {
        return content_type;
    }
    if data.len() >= 12 && &data[4..8] == b"ftyp" {
        return "video/mp4";
    }
    let binary = data
        .iter()
        .any(|&byte| matches!(byte, 0x00..=0x08 | 0x0b | 0x0e..=0x1a | 0x1c..=0x1f));
    if binary {
        "application/octet-stream"
    } else {
        "text/plain; charset=utf-8"
    }
}

#[cfg(test)]
mod tests {
    use axum::body::to_bytes;

    use super::*;

    async fn call(
        content: Content<'_>,
        method: Method,
        headers: &[(&str, &str)],
    ) -> (StatusCode, HeaderMap, Vec<u8>) {
        let mut map = HeaderMap::new();
        for (name, value) in headers {
            map.insert(
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        let response = serve(content, &method, &map).await;
        let status = response.status();
        let headers = response.headers().clone();
        (
            status,
            headers,
            to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap()
                .to_vec(),
        )
    }

    fn content(etag: Option<&'static str>) -> Content<'static> {
        Content {
            source: Source::Memory(Bytes::from_static(b"0123456789")),
            size: 10,
            name: "/a/film.mkv",
            content_type: None,
            modified: Some(1_700_000_000),
            etag,
        }
    }

    const ETAG: &str = "\"1\"";

    #[tokio::test]
    async fn serves_whole_and_ranged_content_with_validators() {
        let (status, headers, body) = call(content(Some(ETAG)), Method::GET, &[]).await;
        assert_eq!(
            (status, body.as_slice()),
            (StatusCode::OK, b"0123456789".as_slice())
        );
        assert_eq!(headers[header::CONTENT_TYPE], "video/x-matroska");
        assert_eq!(headers[header::ETAG], ETAG);
        assert_eq!(
            headers[header::LAST_MODIFIED],
            "Tue, 14 Nov 2023 22:13:20 GMT"
        );

        let (status, headers, body) =
            call(content(Some(ETAG)), Method::POST, &[("range", "bytes=2-4")]).await;
        assert_eq!(
            (status, body.as_slice()),
            (StatusCode::PARTIAL_CONTENT, b"234".as_slice())
        );
        assert_eq!(headers[header::CONTENT_RANGE], "bytes 2-4/10");
    }

    #[tokio::test]
    async fn etag_conditions_follow_go() {
        // Observed from MatriX.145's WebDAV: `If-None-Match: *` is a 304
        // carrying only the ETag.
        let (status, headers, _) =
            call(content(Some(ETAG)), Method::GET, &[("if-none-match", "*")]).await;
        assert_eq!(status, StatusCode::NOT_MODIFIED);
        assert!(headers.contains_key(header::ETAG));
        assert!(!headers.contains_key(header::LAST_MODIFIED));

        let (status, _, _) = call(
            content(Some(ETAG)),
            Method::PUT,
            &[("if-none-match", "W/\"1\"")],
        )
        .await;
        assert_eq!(status, StatusCode::PRECONDITION_FAILED);
        let (status, _, _) = call(
            content(Some(ETAG)),
            Method::GET,
            &[("if-match", "\"2\", \"1\"")],
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (status, _, _) = call(content(None), Method::GET, &[("if-match", "\"1\"")]).await;
        assert_eq!(status, StatusCode::PRECONDITION_FAILED);

        // A stale If-Range serves the whole content.
        let (status, _, _) = call(
            content(Some(ETAG)),
            Method::GET,
            &[("range", "bytes=0-1"), ("if-range", "\"2\"")],
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        // Without an ETag, a 304 keeps Last-Modified.
        let (status, headers, _) = call(
            content(None),
            Method::GET,
            &[("if-modified-since", "Tue, 14 Nov 2023 22:13:20 GMT")],
        )
        .await;
        assert_eq!(status, StatusCode::NOT_MODIFIED);
        assert!(headers.contains_key(header::LAST_MODIFIED));
    }

    #[tokio::test]
    async fn unknown_types_are_sniffed_and_bad_ranges_drop_validators() {
        let mut unknown = content(Some(ETAG));
        unknown.name = "blob";
        let (_, headers, _) = call(unknown, Method::GET, &[]).await;
        assert_eq!(headers[header::CONTENT_TYPE], "text/plain; charset=utf-8");

        let (status, headers, body) =
            call(content(Some(ETAG)), Method::GET, &[("range", "bytes=50-")]).await;
        assert_eq!(status, StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(headers[header::CONTENT_RANGE], "bytes */10");
        assert!(!headers.contains_key(header::ETAG));
        assert_eq!(body, b"invalid range: failed to overlap\n");
    }
}
