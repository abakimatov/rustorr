//! The HTTP side of MatriX.145's DLNA media server (`server/dlna` over
//! `anacrolix/dms`): device description, service descriptions, SOAP
//! control with TorrServer's ContentDirectory, event subscription and the
//! leftovers of the dms file server. SSDP lives in `rustorr-discovery`.

mod content;
mod didl;

pub(crate) use didl::escape as escape_text;
mod soap;

use std::{
    future::Future,
    io,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use axum::{
    Router,
    body::{Body, Bytes, to_bytes},
    extract::State,
    http::{HeaderMap, HeaderName, HeaderValue, Method, Request, Response, StatusCode, header},
    response::IntoResponse,
};
use rand::Rng;
use rustorr_lifecycle::ClientCore;

use crate::{app::go_http_error, file_server::serve_memory};

const SERVER: &str = "Linux/3.4 DLNADOC/1.50 UPnP/1.0 dms/1";
const ROOT_DESC: &str = include_str!("root_desc.xml");
const ROOT_PAGE: &str = include_str!("root_page.html");
const ICONS: [&[u8]; 2] = [
    include_bytes!("icons/48.png"),
    include_bytes!("icons/120.png"),
];
const SCPDS: [(&str, &str); 3] = [
    (
        "/scpd/ContentDirectory.xml",
        include_str!("scpd/ContentDirectory.xml"),
    ),
    (
        "/scpd/ConnectionManager.xml",
        include_str!("scpd/ConnectionManager.xml"),
    ),
    (
        "/scpd/X_MS_MediaReceiverRegistrar.xml",
        include_str!("scpd/X_MS_MediaReceiverRegistrar.xml"),
    ),
];
const XML: &str = "text/xml; charset=\"utf-8\"";

/// What one running DLNA server announces and lists.
pub struct DlnaDevice {
    pub core: Arc<dyn ClientCore>,
    pub friendly_name: String,
    /// `uuid:…`, derived from the friendly name.
    pub udn: String,
    /// The web server's port, which `/play` links point at.
    pub web_port: u16,
    /// Sends event notifications to subscribers.
    pub client: reqwest::Client,
    /// Process start, the modification time of the service descriptions.
    pub started: SystemTime,
}

pub fn dlna_router(device: DlnaDevice) -> Router {
    Router::new()
        .fallback(dispatch)
        .with_state(Arc::new(device))
}

pub async fn serve_dlna(
    listener: crate::Listeners,
    device: DlnaDevice,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> io::Result<()> {
    listener.serve(dlna_router(device), shutdown).await
}

/// Go's `ServeMux` with dms's handlers: every method reaches the handler of
/// its path, and anything unknown gets the root page.
async fn dispatch(State(device): State<Arc<DlnaDevice>>, request: Request<Body>) -> Response<Body> {
    let path = request.uri().path().to_owned();
    let mut response = match path.as_str() {
        "/rootDesc.xml" => root_description(&device),
        "/ctl" => soap::control(&device, request).await,
        "/evt/ContentDirectory" => subscribe(&device, &request),
        "/deviceIcon/0" | "/deviceIcon/1" | "/icon" => {
            let index = usize::from(path.ends_with('1'));
            serve_memory(
                Bytes::from_static(ICONS[index]),
                "image/png",
                None,
                request.method(),
                request.headers(),
            )
            .await
        }
        "/res" => resource(&request),
        // dms serves `<path>.srt` relative to its working directory here;
        // Rustorr does not expose local files, so it is always missing.
        "/subtitle" => go_http_error(StatusCode::NOT_FOUND, "404 page not found"),
        path => match SCPDS.iter().find(|(scpd, _)| *scpd == path) {
            Some((_, document)) => {
                serve_memory(
                    Bytes::from_static(document.as_bytes()),
                    XML,
                    seconds(device.started),
                    request.method(),
                    request.headers(),
                )
                .await
            }
            None => ([(header::CONTENT_TYPE, "text/html")], ROOT_PAGE).into_response(),
        },
    };
    let headers = response.headers_mut();
    headers.insert(HeaderName::from_static("ext"), HeaderValue::from_static(""));
    headers.insert(header::SERVER, HeaderValue::from_static(SERVER));
    response
}

fn seconds(time: SystemTime) -> Option<u64> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .map(|since| since.as_secs())
}

fn root_description(device: &DlnaDevice) -> Response<Body> {
    let xml = ROOT_DESC
        .replace("{friendly_name}", &didl::escape(&device.friendly_name))
        .replace("{udn}", &didl::escape(&device.udn));
    ([(header::CONTENT_TYPE, XML)], xml).into_response()
}

/// dms's `/res` joins the path to an empty root and so never gets an
/// absolute path: always this error.
fn resource(request: &Request<Body>) -> Response<Body> {
    let given = query_value(request.uri().query().unwrap_or_default(), "path");
    let cleaned = clean(&format!("/{given}"));
    go_http_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        &format!("Path must be absolute: {}", &cleaned[1..]),
    )
}

/// Go's `url.Values.Get`: the first value, `+` as a space.
pub(crate) fn query_value(query: &str, name: &str) -> String {
    let decode = |text: &str| {
        percent_encoding::percent_decode_str(&text.replace('+', " "))
            .decode_utf8_lossy()
            .into_owned()
    };
    query
        .split('&')
        .filter_map(|part| {
            let (key, value) = part.split_once('=').unwrap_or((part, ""));
            (decode(key) == name).then(|| decode(value))
        })
        .next()
        .unwrap_or_default()
}

/// Go's `path.Clean`.
pub(crate) fn clean(path: &str) -> String {
    let rooted = path.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.last().is_some_and(|last| *last != "..") {
                    parts.pop();
                } else if !rooted {
                    parts.push("..");
                }
            }
            part => parts.push(part),
        }
    }
    let joined = parts.join("/");
    match (rooted, joined.is_empty()) {
        (true, _) => format!("/{joined}"),
        (false, true) => ".".into(),
        (false, false) => joined,
    }
}

/// `contentDirectoryEventSubHandler`: a new subscription gets an id and,
/// 100 ms later, the initial event; renewals are refused and other methods
/// get an empty `200`.
fn subscribe(device: &DlnaDevice, request: &Request<Body>) -> Response<Body> {
    let headers = request.headers();
    let text = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned()
    };
    if request.method().as_str() != "SUBSCRIBE" {
        return StatusCode::OK.into_response();
    }
    if !text("sid").is_empty() {
        return go_http_error(StatusCode::PRECONDITION_FAILED, "meh");
    }
    let callbacks = callback_urls(&text("callback"));
    let timeout: i64 = text("timeout")
        .strip_prefix("Second-")
        .map(scan_int)
        .unwrap_or(0);
    let sid = random_uuid();
    // The reference measures the remaining time a moment after setting the
    // expiry and truncates: one second less.
    let actual = match timeout {
        1.. => timeout - 1,
        _ => timeout,
    };
    let mut response = StatusCode::OK.into_response();
    let response_headers = response.headers_mut();
    if let Ok(value) = HeaderValue::from_str(&sid) {
        response_headers.insert(HeaderName::from_static("sid"), value);
    }
    if let Ok(value) = HeaderValue::from_str(&format!("Second-{actual}")) {
        response_headers.insert(HeaderName::from_static("timeout"), value);
    }
    let client = device.client.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        initial_event(&client, &callbacks, &sid).await;
    });
    response
}

/// `fmt.Sscanf(s, "%d")`: an optional sign and leading digits.
fn scan_int(text: &str) -> i64 {
    let (sign, digits) = match text.strip_prefix('-') {
        Some(rest) => (-1, rest),
        None => (1, text.strip_prefix('+').unwrap_or(text)),
    };
    let digits: String = digits.chars().take_while(char::is_ascii_digit).collect();
    digits.parse::<i64>().map_or(0, |value| sign * value)
}

/// `ParseCallbackURLs`: every `<…>` in the header.
fn callback_urls(header: &str) -> Vec<reqwest::Url> {
    let mut urls = Vec::new();
    let mut rest = header;
    while let Some(start) = rest.find('<') {
        let Some(length) = rest[start + 1..].find('>') else {
            break;
        };
        if let Ok(url) = reqwest::Url::parse(&rest[start + 1..start + 1 + length]) {
            urls.push(url);
        }
        rest = &rest[start + 1 + length + 1..];
    }
    urls
}

fn random_uuid() -> String {
    let bytes: [u8; 16] = rand::rng().random();
    let hex = |bytes: &[u8]| {
        bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    };
    format!(
        "uuid:{}-{}-{}-{}-{}",
        hex(&bytes[..4]),
        hex(&bytes[4..6]),
        hex(&bytes[6..8]),
        hex(&bytes[8..10]),
        hex(&bytes[10..])
    )
}

async fn initial_event(client: &reqwest::Client, callbacks: &[reqwest::Url], sid: &str) {
    const BODY: &str = "<?xml version=\"1.0\"?>\n<e:propertyset xmlns:e=\"urn:schemas-upnp-org:event-1-0\">\n  <e:property>\n    <SystemUpdateID>0</SystemUpdateID>\n  </e:property>\n</e:propertyset>";
    let Ok(method) = reqwest::Method::from_bytes(b"NOTIFY") else {
        return;
    };
    for url in callbacks {
        let _ = client
            .request(method.clone(), url.clone())
            .header(header::CONTENT_TYPE, XML)
            .header("NT", "upnp:event")
            .header("NTS", "upnp:propchange")
            .header("SID", sid)
            .header("SEQ", "0")
            .timeout(Duration::from_secs(30))
            .body(BODY)
            .send()
            .await;
    }
}

/// Go writes a small response in one piece with its length and streams a
/// larger one chunked; the threshold is its 2 KiB write buffer.
pub(crate) fn go_body(bytes: Vec<u8>) -> Body {
    if bytes.len() <= 2048 {
        Body::from(bytes)
    } else {
        Body::from_stream(futures_once(Bytes::from(bytes)))
    }
}

fn futures_once(
    bytes: Bytes,
) -> impl futures_core::Stream<Item = Result<Bytes, std::convert::Infallible>> {
    async_stream::stream! {
        yield Ok(bytes);
    }
}

/// Reads a request body whole, as the SOAP handler needs it.
pub(crate) async fn body(request: Request<Body>) -> (HeaderMap, Method, String, Vec<u8>) {
    let (parts, body) = request.into_parts();
    let host = parts
        .headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
        .or_else(|| parts.uri.authority().map(ToString::to_string))
        .unwrap_or_default();
    let bytes = to_bytes(body, 4 << 20).await.unwrap_or_default().to_vec();
    (parts.headers, parts.method, host, bytes)
}

#[cfg(test)]
mod tests {
    use axum::body::to_bytes;
    use tower::ServiceExt;

    use super::*;
    use crate::app::tests::playback_core;

    const HASH: &str = "0101010101010101010101010101010101010101";

    fn app() -> Router {
        let (core, _) = playback_core();
        dlna_router(DlnaDevice {
            core,
            friendly_name: "Box & Co".into(),
            udn: "uuid:00000000-0000-0000-0000-000000000001".into(),
            web_port: 8090,
            client: reqwest::Client::new(),
            started: UNIX_EPOCH + Duration::from_secs(1_700_000_000),
        })
    }

    async fn call(request: Request<Body>) -> (StatusCode, HeaderMap, String) {
        let response = app().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, headers, String::from_utf8(body.to_vec()).unwrap())
    }

    async fn soap(service: &str, action: &str, arguments: &str) -> (StatusCode, HeaderMap, String) {
        let body = format!(
            "<?xml version=\"1.0\"?><s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\"><s:Body><u:{action} xmlns:u=\"{service}\">{arguments}</u:{action}></s:Body></s:Envelope>"
        );
        call(
            Request::post("/ctl")
                .header(header::HOST, "dlna.invalid:9080")
                .header("SOAPACTION", format!("\"{service}#{action}\""))
                .body(Body::from(body))
                .unwrap(),
        )
        .await
    }

    fn browse(id: &str, flag: &str) -> String {
        format!(
            "<ObjectID>{id}</ObjectID><BrowseFlag>{flag}</BrowseFlag><StartingIndex>0</StartingIndex><RequestedCount>0</RequestedCount>"
        )
    }

    const CDS: &str = "urn:schemas-upnp-org:service:ContentDirectory:1";

    #[tokio::test]
    async fn the_device_description_names_the_device() {
        let (status, headers, body) =
            call(Request::get("/rootDesc.xml").body(Body::empty()).unwrap()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers[header::SERVER], SERVER);
        assert_eq!(headers["ext"], "");
        assert!(body.contains("<friendlyName>Box &amp; Co</friendlyName>"));
        assert!(body.contains("<UDN>uuid:00000000-0000-0000-0000-000000000001</UDN>"));

        let (_, headers, body) = call(
            Request::get("/scpd/ContentDirectory.xml")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(headers[header::ACCEPT_RANGES], "bytes");
        assert_eq!(
            headers[header::LAST_MODIFIED],
            "Tue, 14 Nov 2023 22:13:20 GMT"
        );
        assert!(body.starts_with("<?xml version=\"1.0\"?>\n<scpd"));

        let (_, headers, body) = call(Request::get("/anything").body(Body::empty()).unwrap()).await;
        assert_eq!(headers[header::CONTENT_TYPE], "text/html");
        assert!(body.starts_with("<form method=\"post\">"));
    }

    #[tokio::test]
    async fn browsing_walks_from_the_root_to_media_files() {
        let (status, _, body) = soap(CDS, "Browse", &browse("0", "BrowseDirectChildren")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("&lt;container id=\"%2FTR\" parentID=\"0\" restricted=\"1\" searchable=\"0\" childCount=\"1\"&gt;"));
        assert!(body.contains("</Result>\n<NumberReturned>1</NumberReturned>\n<TotalMatches>1</TotalMatches>\n<UpdateID>"));

        let (_, _, body) = soap(CDS, "Browse", &browse("%2FTR", "BrowseDirectChildren")).await;
        assert!(body.contains("id=\"%2FTR%2Funcategorized\""));

        let (_, _, body) = soap(
            CDS,
            "Browse",
            &browse(&format!("%2F{HASH}"), "BrowseDirectChildren"),
        )
        .await;
        assert!(body.contains("object.item.videoItem"), "{body}");
        assert!(body.contains(&format!("http://dlna.invalid:8090/play/{HASH}/1")));
        assert!(body.contains("http-get:*:video/mp4:DLNA.ORG_OP=11;DLNA.ORG_CI=0;"));
    }

    #[tokio::test]
    async fn soap_errors_are_upnp_faults() {
        let (status, _, body) =
            soap(CDS, "Browse", &browse("relative", "BrowseDirectChildren")).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(body.contains("<errorCode>701</errorCode>"));
        assert!(body.contains("<errorDescription>bad ObjectID relative</errorDescription>"));

        let (status, _, body) = soap("urn:x:service:AVTransport:1", "Play", "").await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(body.contains("Invalid service: AVTransport"));

        let (status, _, body) = soap(CDS, "Nope", "").await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(body.contains("<errorCode>401</errorCode>"));

        let (status, _, body) = call(
            Request::post("/ctl")
                .header("SOAPACTION", format!("\"{CDS}#Browse\""))
                .body(Body::from("<nope"))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body, "XML syntax error on line 1: unexpected EOF\n");
    }

    #[tokio::test]
    async fn large_soap_answers_are_streamed_without_a_length() {
        let (_, headers, body) = soap(
            "urn:schemas-upnp-org:service:ConnectionManager:1",
            "GetProtocolInfo",
            "",
        )
        .await;
        assert!(body.len() > 2048);
        assert!(!headers.contains_key(header::CONTENT_LENGTH));
        let (_, headers, _) = soap(CDS, "GetSortCapabilities", "").await;
        assert!(headers.contains_key(header::CONTENT_LENGTH));
    }

    #[tokio::test]
    async fn subscriptions_get_an_id_and_renewals_are_refused() {
        let (status, headers, _) = call(
            Request::builder()
                .method("SUBSCRIBE")
                .uri("/evt/ContentDirectory")
                .header("TIMEOUT", "Second-1800")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers["timeout"], "Second-1799");
        assert!(headers["sid"].to_str().unwrap().starts_with("uuid:"));

        let (status, _, body) = call(
            Request::builder()
                .method("SUBSCRIBE")
                .uri("/evt/ContentDirectory")
                .header("SID", "uuid:x")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(
            (status, body.as_str()),
            (StatusCode::PRECONDITION_FAILED, "meh\n")
        );
    }

    #[tokio::test]
    async fn local_files_are_never_served() {
        let (status, _, body) = call(
            Request::get("/res?path=etc/hostname")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body, "Path must be absolute: etc/hostname\n");
        let (status, _, _) = call(
            Request::get("/subtitle?path=x")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[test]
    fn paths_clean_like_go() {
        assert_eq!(clean("/TR/../TR//movie/"), "/TR/movie");
        assert_eq!(clean("/"), "/");
        assert_eq!(clean("relative"), "relative");
        assert_eq!(clean(""), ".");
    }

    #[test]
    fn callbacks_and_timeouts_parse_like_dms() {
        let urls = callback_urls("<http://a/x><http://b/y> junk <bad");
        assert_eq!(urls.len(), 2);
        assert_eq!(scan_int("1800"), 1800);
        assert_eq!(scan_int("infinite"), 0);
        assert_eq!(scan_int("-5x"), -5);
        assert!(random_uuid().starts_with("uuid:"));
        assert_eq!(random_uuid().len(), 41);
    }
}
