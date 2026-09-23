//! A local directory served as gin's `StaticFS(prefix, gin.Dir(root, true))`
//! serves it: gin answers an unopenable path with an empty `404`, everything
//! else is Go's `http.FileServer` — canonicalising redirects, directory
//! listings and `ServeContent` with conditional and range requests.

use std::{
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use axum::{
    body::{Body, Bytes},
    http::{HeaderMap, HeaderValue, Method, Response, StatusCode, header},
    response::IntoResponse,
};

use crate::{
    app::go_http_error,
    serve_content::{self, Content, Source},
};

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

async fn serve_content(
    path: &Path,
    name: &str,
    metadata: &std::fs::Metadata,
    request: &FileRequest<'_>,
) -> Response<Body> {
    serve_content::serve(
        Content {
            source: Source::File(path.to_owned()),
            size: metadata.len(),
            name,
            content_type: None,
            modified: modified_seconds(metadata),
            etag: None,
        },
        request.method,
        request.headers,
    )
    .await
}

/// `http.ServeContent` over bytes in memory, with the content type already
/// chosen; `modified` is `None` for Go's zero time.
pub(crate) async fn serve_memory(
    bytes: Bytes,
    content_type: &str,
    modified: Option<u64>,
    method: &Method,
    headers: &HeaderMap,
) -> Response<Body> {
    let size = bytes.len() as u64;
    serve_content::serve(
        Content {
            source: Source::Memory(bytes),
            size,
            name: "",
            content_type: Some(content_type),
            modified,
            etag: None,
        },
        method,
        headers,
    )
    .await
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
