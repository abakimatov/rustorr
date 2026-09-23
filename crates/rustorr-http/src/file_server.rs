//! A local directory served as gin's `StaticFS(prefix, gin.Dir(root, true))`
//! serves it: gin answers an unopenable path with an empty `404`, everything
//! else is Go's `http.FileServer` — canonicalising redirects, directory
//! listings and `ServeContent` with conditional and range requests.

use std::{
    io::SeekFrom,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use async_stream::try_stream;
use axum::{
    body::{Body, Bytes},
    http::{HeaderMap, HeaderValue, Method, Response, StatusCode, header},
    response::IntoResponse,
};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;

use crate::{
    app::go_http_error,
    range::{self, ByteRange, RangeError},
};

/// Go's `DetectContentType` looks at no more than this many bytes.
const SNIFF_LEN: usize = 512;

pub(crate) struct FileRequest<'a> {
    /// The request path below the mount prefix, percent-decoded and starting
    /// with `/`.
    pub path: &'a str,
    pub query: Option<&'a str>,
    pub method: &'a Method,
    pub headers: &'a HeaderMap,
}

pub(crate) async fn serve(root: &Path, request: FileRequest<'_>) -> Response<Body> {
    // gin opens the path itself first and gives up with a bare 404.
    if tokio::fs::metadata(local(root, request.path))
        .await
        .is_err()
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    if request.path.ends_with("/index.html") {
        return redirect("./", request.query);
    }
    let name = clean(request.path);
    let file = local(root, &name);
    let metadata = match tokio::fs::metadata(&file).await {
        Ok(metadata) => metadata,
        Err(error) => return open_error(&error),
    };
    let url = request.path;
    if metadata.is_dir() {
        if !url.ends_with('/') {
            return redirect(&format!("{}/", base(url)), request.query);
        }
    } else if url.ends_with('/') {
        let base = match base(url) {
            "/" | "." => "",
            base => base,
        };
        return redirect(&format!("../{base}"), request.query);
    }
    if metadata.is_dir() {
        let index = file.join("index.html");
        if let Ok(index_metadata) = tokio::fs::metadata(&index).await
            && !index_metadata.is_dir()
        {
            return serve_content(&index, "index.html", &index_metadata, &request).await;
        }
        return list(&file, &metadata, &request).await;
    }
    let file_name = Path::new(&name)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    serve_content(&file, &file_name, &metadata, &request).await
}

/// `http.Dir.Open`: the cleaned path below `root`.
fn local(root: &Path, path: &str) -> PathBuf {
    let cleaned = clean(&format!("/{path}"));
    root.join(cleaned.trim_start_matches('/'))
}

/// Go's `path.Clean` for rooted paths.
fn clean(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    format!("/{}", parts.join("/"))
}

/// Go's `path.Base`.
fn base(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return if path.is_empty() { "." } else { "/" };
    }
    trimmed.rsplit('/').next().unwrap_or(trimmed)
}

/// `localRedirect`: a relative `Location` with the query kept.
fn redirect(location: &str, query: Option<&str>) -> Response<Body> {
    let location = match query.filter(|query| !query.is_empty()) {
        Some(query) => format!("{location}?{query}"),
        None => location.to_owned(),
    };
    let mut response = StatusCode::MOVED_PERMANENTLY.into_response();
    if let Ok(value) = HeaderValue::from_bytes(location.as_bytes()) {
        response.headers_mut().insert(header::LOCATION, value);
    }
    response
}

/// `toHTTPError` followed by `serveError`.
fn open_error(error: &std::io::Error) -> Response<Body> {
    match error.kind() {
        std::io::ErrorKind::NotFound => go_http_error(StatusCode::NOT_FOUND, "404 page not found"),
        std::io::ErrorKind::PermissionDenied => {
            go_http_error(StatusCode::FORBIDDEN, "403 Forbidden")
        }
        _ => go_http_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "500 Internal Server Error",
        ),
    }
}

async fn list(
    directory: &Path,
    metadata: &std::fs::Metadata,
    request: &FileRequest<'_>,
) -> Response<Body> {
    let modified = modified_seconds(metadata);
    if modified.is_some_and(|modified| unmodified_since(request, modified)) {
        return not_modified(modified);
    }
    let mut entries = Vec::new();
    let mut reader = match tokio::fs::read_dir(directory).await {
        Ok(reader) => reader,
        Err(_) => {
            return go_http_error(StatusCode::INTERNAL_SERVER_ERROR, "Error reading directory");
        }
    };
    loop {
        match reader.next_entry().await {
            Ok(Some(entry)) => {
                let is_dir = entry.file_type().await.is_ok_and(|kind| kind.is_dir());
                let mut name = entry.file_name().as_bytes().to_vec();
                if is_dir {
                    name.push(b'/');
                }
                entries.push(name);
            }
            Ok(None) => break,
            Err(_) => {
                return go_http_error(StatusCode::INTERNAL_SERVER_ERROR, "Error reading directory");
            }
        }
    }
    entries.sort();
    let mut html = String::from(
        "<!doctype html>\n<meta name=\"viewport\" content=\"width=device-width\">\n<pre>\n",
    );
    for name in &entries {
        let display = String::from_utf8_lossy(name);
        html.push_str(&format!(
            "<a href=\"{}\">{}</a>\n",
            href(name),
            html_escape(&display)
        ));
    }
    html.push_str("</pre>\n");
    let length = html.len();
    let body = if request.method == Method::HEAD {
        Body::empty()
    } else {
        Body::from(html)
    };
    let mut response = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(header::CONTENT_LENGTH, length)
        .body(body)
        .expect("valid listing response");
    set_last_modified(response.headers_mut(), modified);
    response
}

/// `url.URL{Path: name}.String()`.
fn href(name: &[u8]) -> String {
    let mut escaped = String::with_capacity(name.len());
    for &byte in name {
        if byte.is_ascii_alphanumeric() || b"-_.~$&+,/:;=@".contains(&byte) {
            escaped.push(char::from(byte));
        } else {
            escaped.push_str(&format!("%{byte:02X}"));
        }
    }
    let first_segment = escaped.split('/').next().unwrap_or_default();
    if first_segment.contains(':') {
        escaped.insert_str(0, "./");
    }
    escaped
}

/// Go's `htmlReplacer`.
pub(crate) fn html_escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&#34;"),
            '\'' => escaped.push_str("&#39;"),
            other => escaped.push(other),
        }
    }
    escaped
}

/// Modification time in whole seconds, as HTTP dates carry it; `None` for
/// Go's zero time.
fn modified_seconds(metadata: &std::fs::Metadata) -> Option<u64> {
    metadata
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|since| since.as_secs())
}

fn set_last_modified(headers: &mut HeaderMap, modified: Option<u64>) {
    if let Some(modified) = modified {
        let date = httpdate::fmt_http_date(UNIX_EPOCH + std::time::Duration::from_secs(modified));
        headers.insert(
            header::LAST_MODIFIED,
            HeaderValue::from_str(&date).expect("an HTTP date is a valid header"),
        );
    }
}

fn header_date(headers: &HeaderMap, name: header::HeaderName) -> Option<u64> {
    let value = headers.get(name)?.to_str().ok()?;
    httpdate::parse_http_date(value)
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|since| since.as_secs())
}

/// `checkIfModifiedSince` returning `condFalse`.
fn unmodified_since(request: &FileRequest<'_>, modified: u64) -> bool {
    if !matches!(*request.method, Method::GET | Method::HEAD) {
        return false;
    }
    header_date(request.headers, header::IF_MODIFIED_SINCE).is_some_and(|since| modified <= since)
}

/// `writeNotModified` without an ETag, which keeps `Last-Modified`.
fn not_modified(modified: Option<u64>) -> Response<Body> {
    let mut response = StatusCode::NOT_MODIFIED.into_response();
    set_last_modified(response.headers_mut(), modified);
    response
}

enum Precondition {
    Failed,
    NotModified,
    /// The `Range` header to honour, if any.
    Proceed(Option<String>),
}

/// `checkPreconditions` for a file without an ETag.
fn preconditions(request: &FileRequest<'_>, modified: Option<u64>) -> Precondition {
    let headers = request.headers;
    let text = |name: header::HeaderName| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
    };
    let if_match = match text(header::IF_MATCH) {
        // Only `*` matches when the response carries no ETag.
        Some(value) if !value.is_empty() => Some(value.split(',').any(|tag| tag.trim() == "*")),
        _ => None,
    };
    let matched = if_match.or_else(|| {
        let since = header_date(headers, header::IF_UNMODIFIED_SINCE)?;
        Some(modified.is_none_or(|modified| modified <= since))
    });
    if matched == Some(false) {
        return Precondition::Failed;
    }
    match text(header::IF_NONE_MATCH) {
        Some(value) if !value.is_empty() => {
            if value.starts_with('*') {
                return if matches!(*request.method, Method::GET | Method::HEAD) {
                    Precondition::NotModified
                } else {
                    Precondition::Failed
                };
            }
        }
        _ => {
            if modified.is_some_and(|modified| unmodified_since(request, modified)) {
                return Precondition::NotModified;
            }
        }
    }
    let range = text(header::RANGE).filter(|value| !value.is_empty());
    let range = match (
        range,
        text(header::IF_RANGE).filter(|value| !value.is_empty()),
    ) {
        (Some(range), Some(if_range)) => {
            let fresh = !if_range.starts_with('"')
                && !if_range.starts_with("W/")
                && httpdate::parse_http_date(if_range)
                    .ok()
                    .and_then(|date| date.duration_since(UNIX_EPOCH).ok())
                    .is_some_and(|date| Some(date.as_secs()) == modified);
            fresh.then(|| range.to_owned())
        }
        (range, _) => range.map(str::to_owned),
    };
    Precondition::Proceed(range)
}

async fn serve_content(
    path: &Path,
    name: &str,
    metadata: &std::fs::Metadata,
    request: &FileRequest<'_>,
) -> Response<Body> {
    let modified = modified_seconds(metadata);
    let range_header = match preconditions(request, modified) {
        Precondition::Failed => {
            let mut response = StatusCode::PRECONDITION_FAILED.into_response();
            set_last_modified(response.headers_mut(), modified);
            return response;
        }
        Precondition::NotModified => return not_modified(modified),
        Precondition::Proceed(range) => range,
    };
    let mut file = match tokio::fs::File::open(path).await {
        Ok(file) => file,
        Err(error) => return open_error(&error),
    };
    let content_type = match content_type_by_extension(name) {
        Some(content_type) => content_type,
        None => {
            let mut buffer = vec![0; SNIFF_LEN];
            let mut filled = 0;
            while filled < SNIFF_LEN {
                match file.read(&mut buffer[filled..]).await {
                    Ok(0) | Err(_) => break,
                    Ok(count) => filled += count,
                }
            }
            if file.seek(SeekFrom::Start(0)).await.is_err() {
                return go_http_error(StatusCode::INTERNAL_SERVER_ERROR, "seeker can't seek");
            }
            sniff(&buffer[..filled]).to_owned()
        }
    };
    let size = metadata.len();
    let ranges = match range_header.map(|value| range::parse(&value, size)) {
        None => Vec::new(),
        Some(Ok(ranges)) if ranges.iter().map(|range| range.len()).sum::<u64>() > size => {
            Vec::new()
        }
        Some(Ok(ranges)) => ranges,
        // An empty file ignores the range rather than failing it.
        Some(Err(RangeError::Unsatisfiable)) if size == 0 => Vec::new(),
        Some(Err(RangeError::Unsatisfiable)) => {
            let mut response = go_http_error(
                StatusCode::RANGE_NOT_SATISFIABLE,
                "invalid range: failed to overlap",
            );
            response.headers_mut().insert(
                header::CONTENT_RANGE,
                HeaderValue::from_str(&format!("bytes */{size}")).expect("valid header"),
            );
            return response;
        }
        Some(Err(RangeError::Invalid)) => {
            return go_http_error(StatusCode::RANGE_NOT_SATISFIABLE, "invalid range");
        }
    };
    let head = request.method == Method::HEAD;
    let mut response = match ranges.as_slice() {
        [] => {
            let body = if head {
                Body::empty()
            } else {
                match file_body(file, 0, size).await {
                    Ok(body) => body,
                    Err(response) => return response,
                }
            };
            let mut response = Response::new(body);
            response
                .headers_mut()
                .insert(header::CONTENT_TYPE, header_value(&content_type));
            response
                .headers_mut()
                .insert(header::CONTENT_LENGTH, size.into());
            response
        }
        [range] => {
            let body = if head {
                Body::empty()
            } else {
                match file_body(file, range.start, range.len()).await {
                    Ok(body) => body,
                    Err(response) => return response,
                }
            };
            let mut response = Response::new(body);
            *response.status_mut() = StatusCode::PARTIAL_CONTENT;
            let headers = response.headers_mut();
            headers.insert(header::CONTENT_TYPE, header_value(&content_type));
            headers.insert(header::CONTENT_LENGTH, range.len().into());
            headers.insert(
                header::CONTENT_RANGE,
                header_value(&format!("bytes {}-{}/{size}", range.start, range.end)),
            );
            response
        }
        ranges => multipart(file, ranges.to_vec(), size, &content_type, head),
    };
    let headers = response.headers_mut();
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    set_last_modified(headers, modified);
    response
}

/// `http.ServeContent` over bytes in memory, with the content type already
/// chosen; `modified` is `None` for Go's zero time, which sends no
/// `Last-Modified` and skips date conditions.
pub(crate) fn serve_memory(
    bytes: Bytes,
    content_type: &str,
    modified: Option<u64>,
    method: &Method,
    headers: &HeaderMap,
) -> Response<Body> {
    let request = FileRequest {
        path: "",
        query: None,
        method,
        headers,
    };
    let range_header = match preconditions(&request, modified) {
        Precondition::Failed => {
            let mut response = StatusCode::PRECONDITION_FAILED.into_response();
            set_last_modified(response.headers_mut(), modified);
            return response;
        }
        Precondition::NotModified => return not_modified(modified),
        Precondition::Proceed(range) => range,
    };
    let size = bytes.len() as u64;
    let ranges = match range_header.map(|value| range::parse(&value, size)) {
        None => Vec::new(),
        Some(Ok(ranges)) if ranges.iter().map(|range| range.len()).sum::<u64>() > size => {
            Vec::new()
        }
        Some(Ok(ranges)) => ranges,
        Some(Err(RangeError::Unsatisfiable)) if size == 0 => Vec::new(),
        Some(Err(RangeError::Unsatisfiable)) => {
            let mut response = go_http_error(
                StatusCode::RANGE_NOT_SATISFIABLE,
                "invalid range: failed to overlap",
            );
            response.headers_mut().insert(
                header::CONTENT_RANGE,
                header_value(&format!("bytes */{size}")),
            );
            return response;
        }
        Some(Err(RangeError::Invalid)) => {
            return go_http_error(StatusCode::RANGE_NOT_SATISFIABLE, "invalid range");
        }
    };
    let head = *method == Method::HEAD;
    let slice = |range: &ByteRange| {
        bytes.slice(
            usize::try_from(range.start).expect("fits")..=usize::try_from(range.end).expect("fits"),
        )
    };
    let mut response = match ranges.as_slice() {
        [] => {
            let mut response = Response::new(if head {
                Body::empty()
            } else {
                Body::from(bytes.clone())
            });
            response
                .headers_mut()
                .insert(header::CONTENT_TYPE, header_value(content_type));
            response
                .headers_mut()
                .insert(header::CONTENT_LENGTH, size.into());
            response
        }
        [range] => {
            let mut response = Response::new(if head {
                Body::empty()
            } else {
                Body::from(slice(range))
            });
            *response.status_mut() = StatusCode::PARTIAL_CONTENT;
            let headers = response.headers_mut();
            headers.insert(header::CONTENT_TYPE, header_value(content_type));
            headers.insert(header::CONTENT_LENGTH, range.len().into());
            headers.insert(
                header::CONTENT_RANGE,
                header_value(&format!("bytes {}-{}/{size}", range.start, range.end)),
            );
            response
        }
        ranges => {
            let boundary = boundary();
            let mut body = Vec::new();
            for range in ranges {
                body.extend_from_slice(
                    format!(
                        "--{boundary}\r\nContent-Range: bytes {}-{}/{size}\r\nContent-Type: {content_type}\r\n\r\n",
                        range.start, range.end
                    )
                    .as_bytes(),
                );
                body.extend_from_slice(&slice(range));
                body.extend_from_slice(b"\r\n");
            }
            body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
            let length = body.len();
            Response::builder()
                .status(StatusCode::PARTIAL_CONTENT)
                .header(
                    header::CONTENT_TYPE,
                    format!("multipart/byteranges; boundary={boundary}"),
                )
                .header(header::CONTENT_LENGTH, length)
                .body(if head {
                    Body::empty()
                } else {
                    Body::from(body)
                })
                .expect("valid multipart response")
        }
    };
    let headers = response.headers_mut();
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    set_last_modified(headers, modified);
    response
}

/// Go draws 30 random bytes; only the 60-character length is observable.
fn boundary() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    format!("{nanos:060x}")
}

fn header_value(text: &str) -> HeaderValue {
    HeaderValue::from_str(text)
        .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"))
}

async fn file_body(
    mut file: tokio::fs::File,
    start: u64,
    length: u64,
) -> Result<Body, Response<Body>> {
    file.seek(SeekFrom::Start(start))
        .await
        .map_err(|error| go_http_error(StatusCode::RANGE_NOT_SATISFIABLE, &error.to_string()))?;
    Ok(Body::from_stream(ReaderStream::new(file.take(length))))
}

/// `multipart/byteranges` as Go's multipart writer frames it.
fn multipart(
    mut file: tokio::fs::File,
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
        let stream: std::pin::Pin<
            Box<dyn futures_core::Stream<Item = Result<Bytes, std::io::Error>> + Send>,
        > = Box::pin(try_stream! {
            for (part, range) in heads.into_iter().zip(ranges) {
                yield Bytes::from(part);
                file.seek(SeekFrom::Start(range.start)).await?;
                let mut remaining = range.len();
                let mut buffer = vec![0; 32 * 1024];
                while remaining > 0 {
                    let want = usize::try_from(remaining.min(buffer.len() as u64)).expect("fits");
                    let count = file.read(&mut buffer[..want]).await?;
                    if count == 0 {
                        Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof))?;
                    }
                    remaining -= count as u64;
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

/// `mime.TypeByExtension`: text types gain a UTF-8 charset.
fn content_type_by_extension(name: &str) -> Option<String> {
    Path::new(name).extension()?;
    let mime = mime_guess::from_path(name).first()?;
    Some(
        if mime.type_() == mime_guess::mime::TEXT && mime.get_param("charset").is_none() {
            format!("{mime}; charset=utf-8")
        } else {
            mime.to_string()
        },
    )
}

/// A subset of Go's `DetectContentType`: common binary signatures, then the
/// text-or-binary decision on control bytes.
fn sniff(data: &[u8]) -> &'static str {
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
    use std::fs;

    use axum::body::to_bytes;

    use super::*;

    async fn get(root: &Path, path: &str, headers: HeaderMap) -> (StatusCode, HeaderMap, Vec<u8>) {
        let response = serve(
            root,
            FileRequest {
                path,
                query: None,
                method: &Method::GET,
                headers: &headers,
            },
        )
        .await;
        let status = response.status();
        let headers = response.headers().clone();
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec();
        (status, headers, body)
    }

    fn fixture() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("readme.txt"), "fixture text\n").unwrap();
        fs::create_dir(root.path().join("Series")).unwrap();
        fs::write(
            root.path().join("Series/Episode 1.txt"),
            "episode fixture\n",
        )
        .unwrap();
        root
    }

    #[tokio::test]
    async fn lists_directories_like_go() {
        let root = fixture();

        let (status, headers, body) = get(root.path(), "/", HeaderMap::new()).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers[header::CONTENT_TYPE], "text/html; charset=utf-8");
        assert!(headers.contains_key(header::LAST_MODIFIED));
        assert_eq!(
            String::from_utf8(body).unwrap(),
            "<!doctype html>\n<meta name=\"viewport\" content=\"width=device-width\">\n<pre>\n\
             <a href=\"Series/\">Series/</a>\n<a href=\"readme.txt\">readme.txt</a>\n</pre>\n"
        );
        let (_, _, body) = get(root.path(), "/Series/", HeaderMap::new()).await;
        assert!(
            String::from_utf8(body)
                .unwrap()
                .contains("<a href=\"Episode%201.txt\">Episode 1.txt</a>")
        );
    }

    #[tokio::test]
    async fn canonicalises_paths_with_relative_redirects() {
        let root = fixture();

        let (status, headers, body) = get(root.path(), "/Series", HeaderMap::new()).await;
        assert_eq!(status, StatusCode::MOVED_PERMANENTLY);
        assert_eq!(headers[header::LOCATION], "Series/");
        assert!(body.is_empty());

        let (status, headers, _) = get(root.path(), "/readme.txt/", HeaderMap::new()).await;
        assert_eq!(status, StatusCode::MOVED_PERMANENTLY);
        assert_eq!(headers[header::LOCATION], "../readme.txt");
    }

    #[tokio::test]
    async fn a_missing_path_is_gins_empty_404() {
        let root = fixture();

        let (status, headers, body) = get(root.path(), "/absent.txt", HeaderMap::new()).await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(!headers.contains_key(header::CONTENT_TYPE));
        assert!(body.is_empty());
    }

    #[tokio::test]
    async fn serves_files_with_ranges_and_conditions() {
        let root = fixture();

        let (status, headers, body) = get(root.path(), "/readme.txt", HeaderMap::new()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers[header::CONTENT_TYPE], "text/plain; charset=utf-8");
        assert_eq!(headers[header::ACCEPT_RANGES], "bytes");
        assert_eq!(body, b"fixture text\n");

        let mut range = HeaderMap::new();
        range.insert(header::RANGE, HeaderValue::from_static("bytes=0-6"));
        let (status, headers, body) = get(root.path(), "/Series/Episode 1.txt", range).await;
        assert_eq!(status, StatusCode::PARTIAL_CONTENT);
        assert_eq!(headers[header::CONTENT_RANGE], "bytes 0-6/16");
        assert_eq!(body, b"episode");

        let mut multi = HeaderMap::new();
        multi.insert(header::RANGE, HeaderValue::from_static("bytes=0-1,4-5"));
        let (status, headers, body) = get(root.path(), "/readme.txt", multi).await;
        assert_eq!(status, StatusCode::PARTIAL_CONTENT);
        let content_type = headers[header::CONTENT_TYPE].to_str().unwrap();
        let boundary = content_type
            .strip_prefix("multipart/byteranges; boundary=")
            .unwrap();
        assert_eq!(boundary.len(), 60);
        assert_eq!(
            headers[header::CONTENT_LENGTH],
            body.len().to_string().as_str()
        );

        let mut beyond = HeaderMap::new();
        beyond.insert(header::RANGE, HeaderValue::from_static("bytes=100-"));
        let (status, headers, body) = get(root.path(), "/readme.txt", beyond).await;
        assert_eq!(status, StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(headers[header::CONTENT_RANGE], "bytes */13");
        assert_eq!(body, b"invalid range: failed to overlap\n");

        let modified = headers_modified(root.path()).await;
        let mut conditional = HeaderMap::new();
        conditional.insert(
            header::IF_MODIFIED_SINCE,
            HeaderValue::from_str(&modified).unwrap(),
        );
        let (status, headers, body) = get(root.path(), "/readme.txt", conditional).await;
        assert_eq!(status, StatusCode::NOT_MODIFIED);
        assert!(headers.contains_key(header::LAST_MODIFIED));
        assert!(body.is_empty());
    }

    async fn headers_modified(root: &Path) -> String {
        let (_, headers, _) = get(root, "/readme.txt", HeaderMap::new()).await;
        headers[header::LAST_MODIFIED].to_str().unwrap().to_owned()
    }

    #[test]
    fn hrefs_escape_like_go_url_paths() {
        assert_eq!(href(b"a b?#%.txt"), "a%20b%3F%23%25.txt");
        assert_eq!(href(b"x:y/"), "./x:y/");
        assert_eq!(href("фильм".as_bytes()), "%D1%84%D0%B8%D0%BB%D1%8C%D0%BC");
        assert_eq!(html_escape("<a&'\">"), "&lt;a&amp;&#39;&#34;&gt;");
    }

    #[test]
    fn cleans_and_bases_paths_like_go() {
        assert_eq!(clean("/a/../../b/./c/"), "/b/c");
        assert_eq!(clean("/"), "/");
        assert_eq!(base("/Series"), "Series");
        assert_eq!(base("/"), "/");
        assert_eq!(base("/readme.txt/"), "readme.txt");
    }
}
