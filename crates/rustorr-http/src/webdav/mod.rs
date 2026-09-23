//! `/dav`: MatriX.145's WebDAV (`server/torrfs/webdav`), `x/net/webdav`'s
//! handler over the read-only torrent file system. Only with `--webdav`,
//! and without HTTP authentication, as in the reference.

mod lock;
mod xml;

use std::{sync::Arc, time::Instant};

use axum::{
    body::{Body, to_bytes},
    extract::State,
    http::{HeaderMap, HeaderValue, Method, Request, Response, StatusCode, header},
    response::IntoResponse,
};
use rustorr_lifecycle::{ClientCore, InfoHash};
use rustorr_vfs::{FsError, Info, Node, TorrentFs};
use tokio::sync::Mutex;

use self::{
    lock::{LockDetails, LockError, MemLs},
    xml::{DAV, LockInfo, Name, Propstat},
};
use crate::{
    ApiError,
    app::AppState,
    dlna::{escape_text, go_body},
    serve_content::{self, Content, Source},
};

const PREFIX: &str = "/dav";
/// Methods gin routes to the handler: `Any` plus the WebDAV extensions.
const ROUTED: [&str; 16] = [
    "GET",
    "POST",
    "PUT",
    "PATCH",
    "HEAD",
    "OPTIONS",
    "DELETE",
    "CONNECT",
    "TRACE",
    "PROPFIND",
    "PROPPATCH",
    "MKCOL",
    "COPY",
    "MOVE",
    "LOCK",
    "UNLOCK",
];
const SUPPORTED_LOCK: &str = "<D:lockentry xmlns:D=\"DAV:\"><D:lockscope><D:exclusive/></D:lockscope><D:locktype><D:write/></D:locktype></D:lockentry>";

/// The WebDAV mount: the file system and its locks.
pub(crate) struct WebDav {
    fs: TorrentFs,
    locks: Mutex<MemLs>,
}

impl WebDav {
    pub(crate) fn new(core: Arc<dyn ClientCore>) -> Self {
        Self {
            fs: TorrentFs::new(core),
            locks: Mutex::new(MemLs::new()),
        }
    }
}

/// `StatusText` with the WebDAV codes.
pub(crate) fn status_text(code: u16) -> &'static str {
    match code {
        207 => "Multi-Status",
        422 => "Unprocessable Entity",
        423 => "Locked",
        424 => "Failed Dependency",
        507 => "Insufficient Storage",
        code => StatusCode::from_u16(code)
            .ok()
            .and_then(|status| status.canonical_reason())
            .unwrap_or_default(),
    }
}

/// The handler's closing `w.WriteHeader(status)` and status text, which Go
/// sniffs as plain text.
fn status(code: u16) -> Response<Body> {
    let status = StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    if status == StatusCode::NO_CONTENT {
        return status.into_response();
    }
    (
        status,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        status_text(code),
    )
        .into_response()
}

/// `cleanWebDAVPath` plus the adapter: a path for the file system.
fn fs_path(name: &str) -> String {
    rustorr_vfs::clean(&format!("/{name}"))
}

/// `url.URL{Path: p}.EscapedPath()`.
pub(crate) fn escape_path(path: &str) -> String {
    let mut escaped = String::with_capacity(path.len());
    for &byte in path.as_bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.~$&+,/:;=@".contains(&byte) {
            escaped.push(char::from(byte));
        } else {
            escaped.push_str(&format!("%{byte:02X}"));
        }
    }
    escaped
}

/// Go's `path.Join` of the prefix and a request path.
fn join(prefix: &str, path: &str) -> String {
    rustorr_vfs::clean(&format!("{prefix}/{path}"))
}

fn header_text<'a>(headers: &'a HeaderMap, name: &str) -> &'a str {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
}

pub(crate) async fn handle(
    State(state): State<AppState>,
    request: Request<Body>,
) -> Response<Body> {
    let Some(dav) = state.webdav.clone() else {
        return ApiError::NotFound.into_response();
    };
    let method = request.method().clone();
    if !ROUTED.contains(&method.as_str()) {
        return ApiError::NotFound.into_response();
    }
    let path = percent_encoding::percent_decode_str(request.uri().path())
        .decode_utf8_lossy()
        .into_owned();
    let Some(request_path) = path.strip_prefix(PREFIX).map(str::to_owned) else {
        return status(404);
    };
    let host = header_text(request.headers(), "host").to_owned();
    let headers = request.headers().clone();
    let body = to_bytes(request.into_body(), 64 << 20)
        .await
        .map(|bytes| bytes.to_vec())
        .unwrap_or_default();
    let context = Context {
        dav: &dav,
        path: request_path,
        headers: &headers,
        host,
        method: method.clone(),
    };
    match method.as_str() {
        "OPTIONS" => context.options().await,
        "GET" | "HEAD" | "POST" => context.get().await,
        "DELETE" => context.delete().await,
        "PUT" => context.put().await,
        "MKCOL" => context.mkcol(body.is_empty()).await,
        "COPY" | "MOVE" => context.copy_move().await,
        "LOCK" => context.lock(&body).await,
        "UNLOCK" => context.unlock().await,
        "PROPFIND" => context.propfind(&body).await,
        "PROPPATCH" => context.proppatch(&body).await,
        _ => status(400),
    }
}

/// What a write operation holds while it runs.
enum Held {
    /// Temporary locks created because the request named none.
    Temporary(Vec<String>),
    /// Locks the request's `If` header confirmed.
    Confirmed(Vec<String>),
}

struct Context<'a> {
    dav: &'a WebDav,
    path: String,
    headers: &'a HeaderMap,
    host: String,
    method: Method,
}

impl Context<'_> {
    async fn stat(&self, path: &str) -> Result<Info, FsError> {
        self.dav.fs.stat(&fs_path(path)).await
    }

    /// `confirmLocks`: without an `If` header a temporary lock on each named
    /// resource; with one, a lock its conditions confirm.
    async fn confirm_locks(&self, source: &str, destination: &str) -> Result<Held, Response<Body>> {
        let mut locks = self.dav.locks.lock().await;
        let now = Instant::now();
        let header = header_text(self.headers, "if");
        if header.is_empty() {
            let mut tokens = Vec::new();
            for name in [source, destination] {
                if name.is_empty() {
                    continue;
                }
                let details = LockDetails {
                    root: name.into(),
                    duration: None,
                    owner_xml: String::new(),
                    zero_depth: true,
                };
                match locks.create(now, details) {
                    Ok(token) => tokens.push(token),
                    Err(error) => {
                        for token in &tokens {
                            let _ = locks.unlock(now, token);
                        }
                        return Err(status(if error == LockError::Locked { 423 } else { 500 }));
                    }
                }
            }
            return Ok(Held::Temporary(tokens));
        }
        let Some(lists) = lock::parse_if(header) else {
            return Err(status(400));
        };
        for list in lists {
            let mut locked_source = source.to_owned();
            if !list.resource_tag.is_empty() {
                let Ok(url) = reqwest::Url::parse(&list.resource_tag) else {
                    continue;
                };
                let authority = match url.port() {
                    Some(port) => format!("{}:{port}", url.host_str().unwrap_or_default()),
                    None => url.host_str().unwrap_or_default().to_owned(),
                };
                if authority != self.host {
                    continue;
                }
                let decoded = percent_encoding::percent_decode_str(url.path())
                    .decode_utf8_lossy()
                    .into_owned();
                let Some(stripped) = decoded.strip_prefix(PREFIX) else {
                    return Err(status(404));
                };
                locked_source = stripped.to_owned();
            }
            match locks.confirm(now, &locked_source, destination, &list.conditions) {
                Ok(held) => return Ok(Held::Confirmed(held)),
                Err(LockError::ConfirmationFailed) => continue,
                Err(_) => return Err(status(500)),
            }
        }
        Err(status(412))
    }

    async fn release(&self, held: Held) {
        let mut locks = self.dav.locks.lock().await;
        match held {
            Held::Temporary(tokens) => {
                let now = Instant::now();
                for token in tokens {
                    let _ = locks.unlock(now, &token);
                }
            }
            Held::Confirmed(roots) => locks.release(&roots),
        }
    }

    async fn options(&self) -> Response<Body> {
        let allow = match self.stat(&self.path).await {
            Ok(info) if info.is_dir => {
                "OPTIONS, LOCK, DELETE, PROPPATCH, COPY, MOVE, UNLOCK, PROPFIND"
            }
            Ok(_) => {
                "OPTIONS, LOCK, GET, HEAD, POST, DELETE, PROPPATCH, COPY, MOVE, UNLOCK, PROPFIND, PUT"
            }
            Err(_) => "OPTIONS, LOCK, PUT, MKCOL",
        };
        let mut response = StatusCode::OK.into_response();
        let headers = response.headers_mut();
        headers.insert(header::ALLOW, HeaderValue::from_static(allow));
        headers.insert("dav", HeaderValue::from_static("1, 2"));
        headers.insert("ms-author-via", HeaderValue::from_static("DAV"));
        response
    }

    async fn get(&self) -> Response<Body> {
        let node = match self.dav.fs.open(&fs_path(&self.path)).await {
            Ok(node) => node,
            Err(_) => return status(404),
        };
        let info = node.info();
        let Node::File { torrent, file, .. } = &node else {
            return status(405);
        };
        let Some(hash) = torrent
            .hash
            .as_deref()
            .and_then(|hash| hash.parse::<InfoHash>().ok())
        else {
            return status(404);
        };
        let etag = etag(&info);
        serve_content::serve(
            Content {
                source: Source::Torrent {
                    core: Arc::clone(self.dav.fs.core()),
                    hash,
                    index: file.id,
                },
                size: info.size,
                name: &self.path,
                content_type: None,
                modified: u64::try_from(info.mtime).ok(),
                etag: Some(&etag),
            },
            &self.method,
            self.headers,
        )
        .await
    }

    async fn delete(&self) -> Response<Body> {
        let held = match self.confirm_locks(&self.path, "").await {
            Ok(held) => held,
            Err(response) => return response,
        };
        let code = match self.stat(&self.path).await {
            Err(FsError::NotFound) => 404,
            // Found or not, removal is refused.
            _ => 405,
        };
        self.release(held).await;
        status(code)
    }

    /// `PUT`: the read-only file system refuses `OpenFile` with `O_CREATE`,
    /// which the handler maps to 404.
    async fn put(&self) -> Response<Body> {
        let held = match self.confirm_locks(&self.path, "").await {
            Ok(held) => held,
            Err(response) => return response,
        };
        self.release(held).await;
        status(404)
    }

    async fn mkcol(&self, empty: bool) -> Response<Body> {
        let held = match self.confirm_locks(&self.path, "").await {
            Ok(held) => held,
            Err(response) => return response,
        };
        let code = if empty { 405 } else { 415 };
        self.release(held).await;
        status(code)
    }

    async fn copy_move(&self) -> Response<Body> {
        let destination = header_text(self.headers, "destination");
        if destination.is_empty() {
            return status(400);
        }
        let (host, path) = match split_url(destination) {
            Some(parts) => parts,
            None => return status(400),
        };
        if !host.is_empty() && host != self.host {
            return status(502);
        }
        let Some(destination) = path.strip_prefix(PREFIX).map(str::to_owned) else {
            return status(404);
        };
        if destination.is_empty() {
            return status(502);
        }
        if destination == self.path {
            return status(403);
        }
        let copy = self.method.as_str() == "COPY";
        let held = match if copy {
            self.confirm_locks("", &destination).await
        } else {
            self.confirm_locks(&self.path, &destination).await
        } {
            Ok(held) => held,
            Err(response) => return response,
        };
        let depth = header_text(self.headers, "depth");
        let code = if copy {
            if !depth.is_empty() && !matches!(parse_depth(depth), Some(0) | Some(-1)) {
                400
            } else {
                match self.stat(&self.path).await {
                    Err(FsError::NotFound) => 404,
                    Err(_) => 500,
                    Ok(_) => match self.stat(&destination).await {
                        Err(FsError::NotFound) => 403,
                        Err(_) => 403,
                        Ok(_) if header_text(self.headers, "overwrite") == "F" => 412,
                        Ok(_) => 403,
                    },
                }
            }
        } else if !depth.is_empty() && parse_depth(depth) != Some(-1) {
            400
        } else {
            match self.stat(&destination).await {
                Err(FsError::NotFound) => 403,
                Err(_) => 403,
                Ok(_) if header_text(self.headers, "overwrite") == "T" => 403,
                Ok(_) => 412,
            }
        };
        self.release(held).await;
        status(code)
    }

    async fn lock(&self, body: &[u8]) -> Response<Body> {
        let Ok(duration) = lock::parse_timeout(header_text(self.headers, "timeout")) else {
            return status(400);
        };
        let info = match xml::lockinfo(body) {
            Ok(LockInfo::Unsupported) => return status(501),
            Ok(info) => info,
            Err(()) => return status(400),
        };
        let now = Instant::now();
        // The third value: whether a new lock (and its token header) was made.
        let (token, details, new_lock) = match info {
            LockInfo::Empty => {
                let Some(lists) = lock::parse_if(header_text(self.headers, "if")) else {
                    return status(400);
                };
                let token = match lists.as_slice() {
                    [list] if list.conditions.len() == 1 => list.conditions[0].token.clone(),
                    _ => String::new(),
                };
                if token.is_empty() {
                    return status(400);
                }
                let refreshed = self.dav.locks.lock().await.refresh(now, &token, duration);
                match refreshed {
                    Ok(details) => (token, details, false),
                    Err(LockError::NoSuchLock) => return status(412),
                    Err(_) => return status(500),
                }
            }
            LockInfo::Exclusive { owner_xml } => {
                let depth = header_text(self.headers, "depth");
                let depth = if depth.is_empty() {
                    Some(-1)
                } else {
                    parse_depth(depth)
                };
                if !matches!(depth, Some(0) | Some(-1)) {
                    return status(400);
                }
                let details = LockDetails {
                    root: self.path.clone(),
                    duration,
                    owner_xml,
                    zero_depth: depth == Some(0),
                };
                let created = self.dav.locks.lock().await.create(now, details.clone());
                let token = match created {
                    Ok(token) => token,
                    Err(LockError::Locked) => return status(423),
                    Err(_) => return status(500),
                };
                if self.stat(&self.path).await.is_err() {
                    // Creating the missing file is refused by the read-only
                    // file system; the lock is released again.
                    let _ = self.dav.locks.lock().await.unlock(now, &token);
                    return status(500);
                }
                (token, details, true)
            }
            LockInfo::Unsupported => return status(501),
        };
        let body = xml::lock_body(
            &token,
            &details.root,
            &details.owner_xml,
            details.duration.map(|duration| duration.as_secs()),
            details.zero_depth,
        );
        // A read-only file system never creates the locked resource, so the
        // status stays 200.
        let mut response = Response::new(Body::from(body));
        let headers = response.headers_mut();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/xml; charset=utf-8"),
        );
        if new_lock && let Ok(value) = HeaderValue::from_str(&format!("<{token}>")) {
            headers.insert("lock-token", value);
        }
        response
    }

    async fn unlock(&self) -> Response<Body> {
        let token = header_text(self.headers, "lock-token");
        let Some(token) = token
            .strip_prefix('<')
            .and_then(|rest| rest.strip_suffix('>'))
            .filter(|_| token.len() >= 2)
        else {
            return status(400);
        };
        let result = self.dav.locks.lock().await.unlock(Instant::now(), token);
        match result {
            Ok(()) => status(204),
            Err(LockError::Locked) => status(423),
            Err(LockError::NoSuchLock) => status(409),
            Err(LockError::ConfirmationFailed) => status(500),
        }
    }

    async fn propfind(&self, body: &[u8]) -> Response<Body> {
        let info = match self.stat(&self.path).await {
            Ok(info) => info,
            Err(FsError::NotFound) => return status(404),
            Err(FsError::Invalid) => return status(405),
        };
        let depth = header_text(self.headers, "depth");
        let depth = if depth.is_empty() {
            Some(-1)
        } else {
            parse_depth(depth)
        };
        let Some(depth) = depth else {
            return status(400);
        };
        let Ok(request) = xml::propfind(body) else {
            return status(400);
        };
        let mut responses = String::new();
        self.walk(&request, depth, self.path.clone(), info, &mut responses)
            .await;
        let document = xml::multistatus(&responses);
        let mut response = Response::new(go_body(document.into_bytes()));
        *response.status_mut() = StatusCode::MULTI_STATUS;
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/xml; charset=utf-8"),
        );
        response
    }

    /// `walkFS` with the PROPFIND callback: the resource, then its children
    /// to the requested depth.
    async fn walk(
        &self,
        request: &xml::Propfind,
        depth: i32,
        path: String,
        info: Info,
        out: &mut String,
    ) {
        let mut stack = vec![(path, info, depth)];
        while let Some((path, info, depth)) = stack.pop() {
            let Some(propstats) = self.propstats(request, &path, &info).await else {
                continue;
            };
            let mut href = join(PREFIX, &path);
            if href != "/" && info.is_dir {
                href.push('/');
            }
            out.push_str(&xml::response(&escape_path(&href), &propstats));
            if !info.is_dir || depth == 0 {
                continue;
            }
            let depth = if depth == 1 { 0 } else { depth };
            let Ok(node) = self.dav.fs.open(&fs_path(&path)).await else {
                continue;
            };
            let Ok(children) = self.dav.fs.read_dir(&node).await else {
                continue;
            };
            // Depth-first in listing order: push in reverse.
            for child in children.iter().rev() {
                let child_path = rustorr_vfs::clean(&format!("{path}/{}", child.info().name));
                if let Ok(child_info) = self.stat(&child_path).await {
                    stack.push((child_path, child_info, depth));
                }
            }
        }
    }

    /// `props`, `allprop` or `propnames` for one resource.
    async fn propstats(
        &self,
        request: &xml::Propfind,
        path: &str,
        info: &Info,
    ) -> Option<Vec<Propstat>> {
        let live = live_names(info.is_dir);
        if request.propname {
            return Some(vec![Propstat {
                props: live.into_iter().map(|name| (name, String::new())).collect(),
                status: 200,
                error: None,
            }]);
        }
        let names: Vec<Name> = if request.allprop {
            let mut names = live;
            for extra in request.include.iter().flatten() {
                if !names.contains(extra) {
                    names.push(extra.clone());
                }
            }
            names
        } else {
            request.prop.clone().unwrap_or_default()
        };
        let mut found = Propstat {
            props: Vec::new(),
            status: 200,
            error: None,
        };
        let mut missing = Propstat {
            props: Vec::new(),
            status: 404,
            error: None,
        };
        for name in names {
            match self.live_value(&name, path, info).await {
                Some(value) => found.props.push((name, value)),
                None => missing.props.push((name, String::new())),
            }
        }
        Some(make_propstats(found, missing))
    }

    /// The live property's value, `None` for a property this resource does
    /// not have.
    async fn live_value(&self, name: &Name, path: &str, info: &Info) -> Option<String> {
        if name.space != DAV {
            return None;
        }
        let dir = info.is_dir;
        Some(match name.local.as_str() {
            "resourcetype" if dir => "<D:collection xmlns:D=\"DAV:\"/>".into(),
            "resourcetype" => String::new(),
            "displayname" => {
                if lock::slash_clean(path) == "/" {
                    String::new()
                } else {
                    escape_display(&info.name)
                }
            }
            "getcontentlength" if !dir => info.size.to_string(),
            "getlastmodified" => httpdate::fmt_http_date(
                std::time::UNIX_EPOCH
                    + std::time::Duration::from_secs(u64::try_from(info.mtime).unwrap_or(0)),
            ),
            "getcontenttype" if !dir => self.content_type(path).await?,
            "getetag" if !dir => etag(info),
            "supportedlock" => SUPPORTED_LOCK.into(),
            _ => return None,
        })
    }

    /// `findContentType`: by the path's extension, else sniffed from the
    /// first bytes of the file.
    async fn content_type(&self, path: &str) -> Option<String> {
        if let Some(kind) = crate::media_type::by_extension(path) {
            return Some(kind);
        }
        let Ok(Node::File { torrent, file, .. }) = self.dav.fs.open(&fs_path(path)).await else {
            return None;
        };
        let hash = torrent.hash.as_deref()?.parse::<InfoHash>().ok()?;
        let source = Source::Torrent {
            core: Arc::clone(self.dav.fs.core()),
            hash,
            index: file.id,
        };
        Some(serve_content::sniff_source(&source, file.length).await)
    }

    async fn proppatch(&self, body: &[u8]) -> Response<Body> {
        let held = match self.confirm_locks(&self.path, "").await {
            Ok(held) => held,
            Err(response) => return response,
        };
        let response = self.proppatch_locked(body).await;
        self.release(held).await;
        response
    }

    async fn proppatch_locked(&self, body: &[u8]) -> Response<Body> {
        match self.stat(&self.path).await {
            Err(FsError::NotFound) => return status(404),
            Err(FsError::Invalid) => return status(405),
            Ok(_) => {}
        }
        let Ok(names) = xml::proppatch(body) else {
            return status(400);
        };
        let live = |name: &Name| name.space == DAV && LIVE.contains(&name.local.as_str());
        if !names.iter().any(live) {
            // Dead properties need a writable file, which is refused.
            return status(500);
        }
        let mut forbidden = Propstat {
            props: Vec::new(),
            status: 403,
            error: Some("<D:cannot-modify-protected-property xmlns:D=\"DAV:\"/>"),
        };
        let mut failed = Propstat {
            props: Vec::new(),
            status: 424,
            error: None,
        };
        for name in names {
            if live(&name) {
                forbidden.props.push((name, String::new()));
            } else {
                failed.props.push((name, String::new()));
            }
        }
        let href = escape_path(&format!("{PREFIX}{}", self.path));
        let document = xml::multistatus(&xml::response(&href, &make_propstats(forbidden, failed)));
        let mut response = Response::new(go_body(document.into_bytes()));
        *response.status_mut() = StatusCode::MULTI_STATUS;
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/xml; charset=utf-8"),
        );
        response
    }
}

/// Every live property `x/net/webdav` knows, found or not.
const LIVE: [&str; 10] = [
    "resourcetype",
    "displayname",
    "getcontentlength",
    "getlastmodified",
    "creationdate",
    "getcontentlanguage",
    "getcontenttype",
    "getetag",
    "lockdiscovery",
    "supportedlock",
];

/// The live properties with a value for a file or a directory.
fn live_names(dir: bool) -> Vec<Name> {
    let mut names = vec![
        Name::dav("resourcetype"),
        Name::dav("displayname"),
        Name::dav("getlastmodified"),
        Name::dav("supportedlock"),
    ];
    if !dir {
        names.extend([
            Name::dav("getcontentlength"),
            Name::dav("getcontenttype"),
            Name::dav("getetag"),
        ]);
    }
    names
}

/// `makePropstats`: non-empty groups only, a bare 200 when both are empty.
fn make_propstats(first: Propstat, second: Propstat) -> Vec<Propstat> {
    let mut propstats: Vec<Propstat> = [first, second]
        .into_iter()
        .filter(|propstat| !propstat.props.is_empty())
        .collect();
    if propstats.is_empty() {
        propstats.push(Propstat {
            props: Vec::new(),
            status: 200,
            error: None,
        });
    }
    propstats
}

/// `escapeXML`: names of plain characters stay as they are.
fn escape_display(name: &str) -> String {
    let plain = name.bytes().all(|byte| {
        byte == b' ' || byte == b'_' || (b'+'..=b'9').contains(&byte) || byte.is_ascii_alphabetic()
    });
    if plain {
        name.into()
    } else {
        escape_text(name)
    }
}

/// `findETag`: the modification time in nanoseconds and the size, in hex.
fn etag(info: &Info) -> String {
    let nanos = i128::from(info.mtime) * 1_000_000_000;
    format!("\"{nanos:x}{:x}\"", info.size)
}

/// `parseDepth`: `-1` is infinity; `None` is invalid.
fn parse_depth(value: &str) -> Option<i32> {
    match value {
        "0" => Some(0),
        "1" => Some(1),
        "infinity" => Some(-1),
        _ => None,
    }
}

/// `url.Parse` for a `Destination`: its host (with port) and decoded path.
fn split_url(text: &str) -> Option<(String, String)> {
    if text.contains("://") {
        let url = reqwest::Url::parse(text).ok()?;
        let host = match url.port() {
            Some(port) => format!("{}:{port}", url.host_str().unwrap_or_default()),
            None => url.host_str().unwrap_or_default().to_owned(),
        };
        let path = percent_encoding::percent_decode_str(url.path())
            .decode_utf8_lossy()
            .into_owned();
        return Some((host, path));
    }
    let path = text.split(['?', '#']).next().unwrap_or_default();
    Some((
        String::new(),
        percent_encoding::percent_decode_str(path)
            .decode_utf8_lossy()
            .into_owned(),
    ))
}
