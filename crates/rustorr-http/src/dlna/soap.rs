//! `/ctl`: dms's SOAP control endpoint and its three services.

use axum::{
    body::Body,
    http::{Request, Response, StatusCode, header},
};
use quick_xml::{
    NsReader, XmlVersion,
    events::Event,
    name::{Namespace, ResolveResult},
};

use super::{DlnaDevice, XML, body, content, didl::escape, go_body};
use crate::app::go_http_error;

const ENVELOPE_NS: &[u8] = b"http://schemas.xmlsoap.org/soap/envelope/";
const FEATURE_LIST: &str = include_str!("feature_list.xml");
const PROTOCOL_INFO: &str = include_str!("protocol_info.txt");

/// `upnp.Error`: a UPnP error code and description.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UpnpError {
    pub code: u32,
    pub description: String,
}

impl UpnpError {
    pub(crate) fn new(code: u32, description: impl Into<String>) -> Self {
        Self {
            code,
            description: description.into(),
        }
    }

    fn invalid_action() -> Self {
        Self::new(401, "Invalid Action")
    }
}

/// A parsed `SOAPACTION` header.
#[derive(Debug, Default, PartialEq, Eq)]
struct SoapAction {
    auth: String,
    kind: String,
    version: u64,
    action: String,
}

impl SoapAction {
    fn urn(&self) -> String {
        format!("urn:{}:service:{}:{}", self.auth, self.kind, self.version)
    }
}

/// `upnp.ParseActionHTTPHeader`: anything not `"<urn>#<action>"` yields the
/// empty action without an error; a malformed service URN is an error.
fn parse_action(header: &str) -> Result<SoapAction, String> {
    let Some(inner) = header
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .filter(|_| header.len() >= 3)
    else {
        return Ok(SoapAction::default());
    };
    let Some(hash) = inner.rfind('#') else {
        return Ok(SoapAction::default());
    };
    let (urn, action) = (&inner[..hash], &inner[hash + 1..]);
    let mut parsed = parse_service_type(urn)?;
    parsed.action = action.to_owned();
    Ok(parsed)
}

/// `^urn:(.*):service:(\w+):(\d+)$`, the authority matched greedily.
fn parse_service_type(urn: &str) -> Result<SoapAction, String> {
    let fail = || urn.to_owned();
    let rest = urn.strip_prefix("urn:").ok_or_else(fail)?;
    let word = |text: &str| {
        !text.is_empty()
            && text
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    };
    for (at, _) in rest
        .match_indices(":service:")
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
    {
        let tail = &rest[at + ":service:".len()..];
        let Some((kind, version)) = tail.split_once(':') else {
            continue;
        };
        if word(kind) && !version.is_empty() && version.bytes().all(|byte| byte.is_ascii_digit()) {
            return Ok(SoapAction {
                auth: rest[..at].to_owned(),
                kind: kind.to_owned(),
                version: parse_uint_base0(version)?,
                action: String::new(),
            });
        }
    }
    Err(fail())
}

/// `strconv.ParseUint(s, 0, 0)` on decimal digits: a leading zero means
/// octal.
fn parse_uint_base0(digits: &str) -> Result<u64, String> {
    let parsed = if digits.len() > 1 && digits.starts_with('0') {
        u64::from_str_radix(&digits[1..], 8)
    } else {
        digits.parse::<u64>()
    };
    parsed.map_err(|_| format!("strconv.ParseUint: parsing \"{digits}\": invalid syntax"))
}

pub(crate) async fn control(device: &DlnaDevice, request: Request<Body>) -> Response<Body> {
    let (headers, _method, host, bytes) = body(request).await;
    let header_value = headers
        .get("soapaction")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let action = match parse_action(header_value) {
        Ok(action) => action,
        Err(message) => return go_http_error(StatusCode::BAD_REQUEST, &message),
    };
    let arguments = match envelope_body(&bytes) {
        Ok(arguments) => arguments,
        Err(message) => return go_http_error(StatusCode::BAD_REQUEST, &message),
    };
    let result = match action.kind.as_str() {
        "ContentDirectory" => content_directory(device, &action.action, &arguments, &host).await,
        "ConnectionManager" => connection_manager(&action.action),
        "X_MS_MediaReceiverRegistrar" => registrar(device, &action.action),
        kind => Err(UpnpError::new(401, format!("Invalid service: {kind}"))),
    };
    let (status, inner) = match result {
        Ok(arguments) => (StatusCode::OK, response_xml(&action, &arguments)),
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, fault_xml(&error)),
    };
    let text = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\" standalone=\"yes\"?><s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\"><s:Body>{inner}</s:Body></s:Envelope>"
    )
    .replace("&#34;", "\"");
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, XML)
        .body(go_body(text.into_bytes()))
        .expect("valid SOAP response")
}

/// `marshalSOAPResponse`: arguments as `xml.MarshalIndent` writes a slice,
/// one element per line.
fn response_xml(action: &SoapAction, arguments: &[(&str, String)]) -> String {
    let arguments: Vec<String> = arguments
        .iter()
        .map(|(name, value)| format!("<{name}>{}</{name}>", escape(value)))
        .collect();
    format!(
        "<u:{0}Response xmlns:u=\"{1}\">{2}</u:{0}Response>",
        action.action,
        action.urn(),
        arguments.join("\n")
    )
}

fn fault_xml(error: &UpnpError) -> String {
    format!(
        "<Fault xmlns=\"http://schemas.xmlsoap.org/soap/envelope/\">\n  <faultcode>http://schemas.xmlsoap.org/soap/envelope/:Client</faultcode>\n  <faultstring>UPnPError</faultstring>\n  <detail>\n    <UPnPError xmlns=\"urn:schemas-upnp-org:control-1-0\">\n      <errorCode>{}</errorCode>\n      <errorDescription>{}</errorDescription>\n    </UPnPError>\n  </detail>\n</Fault>",
        error.code,
        escape(&error.description)
    )
}

type Arguments = Vec<(&'static str, String)>;

async fn content_directory(
    device: &DlnaDevice,
    action: &str,
    arguments: &[u8],
    host: &str,
) -> Result<Arguments, UpnpError> {
    let update_id = std::process::id().to_string();
    match action {
        "GetSystemUpdateID" => Ok(vec![("Id", update_id)]),
        "GetSortCapabilities" => Ok(vec![("SortCaps", "dc:title".into())]),
        "GetSearchCapabilities" => Ok(vec![("SearchCaps", String::new())]),
        "X_GetFeatureList" => Ok(vec![("FeatureList", FEATURE_LIST.into())]),
        "X_SetBookmark" => Ok(Vec::new()),
        "Browse" => {
            let browse =
                Browse::parse(arguments).map_err(|message| UpnpError::new(501, message))?;
            let (result, returned, total) = content::browse(
                device,
                &browse.object_id,
                &browse.flag,
                browse.start,
                browse.count,
                host,
            )
            .await?;
            Ok(vec![
                ("Result", result),
                ("NumberReturned", returned.to_string()),
                ("TotalMatches", total.to_string()),
                ("UpdateID", update_id),
            ])
        }
        _ => Err(UpnpError::invalid_action()),
    }
}

fn connection_manager(action: &str) -> Result<Arguments, UpnpError> {
    match action {
        // dms registers this action with a leading dot, so clients never
        // reach it.
        ".GetCurrentConnectionInfo" => Ok(vec![
            ("ConnectionID", "0".into()),
            ("RcsID", "-1".into()),
            ("AVTransportID", "-1".into()),
            ("ProtocolInfo", String::new()),
            ("PeerConnectionManager", String::new()),
            ("PeerConnectionID", "-1".into()),
            ("Direction", "Output".into()),
            ("Status", "OK".into()),
        ]),
        "GetCurrentConnectionIDs" => Ok(vec![("ConnectionIDs", String::new())]),
        "GetProtocolInfo" => Ok(vec![
            ("Source", PROTOCOL_INFO.into()),
            ("Sink", String::new()),
        ]),
        _ => Err(UpnpError::invalid_action()),
    }
}

fn registrar(device: &DlnaDevice, action: &str) -> Result<Arguments, UpnpError> {
    match action {
        "IsAuthorized" | "IsValidated" => Ok(vec![("Result", "1".into())]),
        "RegisterDevice" => Ok(vec![("RegistrationRespMsg", device.udn.clone())]),
        _ => Err(UpnpError::invalid_action()),
    }
}

/// The arguments of a `Browse` action.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Browse {
    pub object_id: String,
    pub flag: String,
    pub start: i64,
    pub count: i64,
}

impl Browse {
    /// `xml.Unmarshal` into dms's `browse` struct: children of the first
    /// element matched by local name.
    fn parse(xml: &[u8]) -> Result<Self, String> {
        let mut reader = NsReader::from_reader(xml);
        let mut browse = Browse::default();
        let mut depth = 0usize;
        let mut field: Option<Vec<u8>> = None;
        let mut text = String::new();
        let mut started = false;
        loop {
            let event = reader
                .read_event()
                .map_err(|error| syntax(xml, &reader, &error))?;
            match event {
                Event::Start(start) => {
                    depth += 1;
                    started = true;
                    if depth == 2 {
                        field = Some(start.local_name().as_ref().to_vec());
                        text.clear();
                    }
                }
                Event::Empty(start) => {
                    started = true;
                    if depth == 1 {
                        browse.set(start.local_name().as_ref(), "")?;
                    } else if depth == 0 {
                        return Ok(browse);
                    }
                }
                Event::Text(value) if depth == 2 => {
                    text.push_str(
                        &value
                            .xml_content(XmlVersion::Implicit1_0)
                            .map_err(|error| error.to_string())?,
                    );
                }
                Event::GeneralRef(reference) if depth == 2 => {
                    let name = String::from_utf8_lossy(&reference).into_owned();
                    text.push_str(&resolve_reference(&name));
                }
                Event::CData(value) if depth == 2 => {
                    text.push_str(&String::from_utf8_lossy(&value));
                }
                Event::End(_) => {
                    if depth == 2
                        && let Some(name) = field.take()
                    {
                        browse.set(&name, &text)?;
                    }
                    depth = depth.saturating_sub(1);
                    if depth == 0 && started {
                        return Ok(browse);
                    }
                }
                Event::Eof => {
                    return Err(if started {
                        format!(
                            "XML syntax error on line {}: unexpected EOF",
                            line(xml, xml.len())
                        )
                    } else {
                        "EOF".into()
                    });
                }
                _ => {}
            }
        }
    }

    fn set(&mut self, name: &[u8], value: &str) -> Result<(), String> {
        let int = |value: &str| -> Result<i64, String> {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                return Ok(0);
            }
            trimmed.parse::<i64>().map_err(|error| {
                let reason = match error.kind() {
                    std::num::IntErrorKind::PosOverflow | std::num::IntErrorKind::NegOverflow => {
                        "value out of range"
                    }
                    _ => "invalid syntax",
                };
                format!("strconv.ParseInt: parsing \"{trimmed}\": {reason}")
            })
        };
        match name {
            b"ObjectID" => self.object_id = value.to_owned(),
            b"BrowseFlag" => self.flag = value.to_owned(),
            b"StartingIndex" => self.start = int(value)?,
            b"RequestedCount" => self.count = int(value)?,
            _ => {}
        }
        Ok(())
    }
}

fn resolve_reference(name: &str) -> String {
    match name {
        "lt" => "<".into(),
        "gt" => ">".into(),
        "amp" => "&".into(),
        "apos" => "'".into(),
        "quot" => "\"".into(),
        _ => {
            let code = name
                .strip_prefix("#x")
                .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                .or_else(|| {
                    name.strip_prefix('#')
                        .and_then(|decimal| decimal.parse().ok())
                });
            code.and_then(char::from_u32)
                .map(String::from)
                .unwrap_or_default()
        }
    }
}

/// The raw content of `<Envelope><Body>` as dms decodes the request.
fn envelope_body(xml: &[u8]) -> Result<Vec<u8>, String> {
    let mut reader = NsReader::from_reader(xml);
    reader.config_mut().check_end_names = true;
    // Up to the root element.
    loop {
        let (namespace, event) = match reader.read_resolved_event() {
            Ok(resolved) => resolved,
            Err(error) => return Err(syntax(xml, &reader, &error)),
        };
        let empty = matches!(event, Event::Empty(_));
        match event {
            Event::Start(start) | Event::Empty(start) => {
                let local = String::from_utf8_lossy(start.local_name().as_ref()).into_owned();
                let is_envelope = matches!(namespace, ResolveResult::Bound(Namespace(ns)) if ns == ENVELOPE_NS)
                    && local == "Envelope";
                if !is_envelope {
                    return Err(format!(
                        "expected element type <Envelope> but have <{local}>"
                    ));
                }
                if empty {
                    return Ok(Vec::new());
                }
                break;
            }
            Event::Eof => return Err("EOF".into()),
            _ => {}
        }
    }
    let mut action = Vec::new();
    loop {
        let (namespace, event) = match reader.read_resolved_event() {
            Ok(resolved) => resolved,
            Err(error) => return Err(syntax(xml, &reader, &error)),
        };
        match event {
            Event::Start(start) => {
                let is_body = matches!(namespace, ResolveResult::Bound(Namespace(ns)) if ns == ENVELOPE_NS)
                    && start.local_name().as_ref() == b"Body";
                let name = start.name().as_ref().to_vec();
                let span = reader
                    .read_to_end(quick_xml::name::QName(&name))
                    .map_err(|error| syntax(xml, &reader, &error))?;
                if is_body {
                    let start = usize::try_from(span.start).unwrap_or(0);
                    let end = usize::try_from(span.end).unwrap_or(0);
                    action = xml.get(start..end).unwrap_or_default().to_vec();
                }
            }
            Event::End(_) => return Ok(action),
            Event::Eof => {
                return Err(format!(
                    "XML syntax error on line {}: unexpected EOF",
                    line(xml, xml.len())
                ));
            }
            _ => {}
        }
    }
}

/// An `encoding/xml` syntax error: the line of the failure and Go's wording
/// where it is known.
fn syntax(xml: &[u8], reader: &NsReader<&[u8]>, error: &quick_xml::Error) -> String {
    use quick_xml::errors::{IllFormedError, SyntaxError};
    let position = usize::try_from(reader.error_position()).unwrap_or(xml.len());
    let message = match error {
        quick_xml::Error::Syntax(
            SyntaxError::UnclosedTag
            | SyntaxError::UnclosedComment
            | SyntaxError::UnclosedCData
            | SyntaxError::UnclosedDoctype
            | SyntaxError::UnclosedPI
            | SyntaxError::UnclosedXmlDecl
            | SyntaxError::UnclosedSingleQuotedAttributeValue
            | SyntaxError::UnclosedDoubleQuotedAttributeValue,
        )
        | quick_xml::Error::IllFormed(IllFormedError::MissingEndTag(_)) => {
            return format!(
                "XML syntax error on line {}: unexpected EOF",
                line(xml, xml.len())
            );
        }
        quick_xml::Error::IllFormed(IllFormedError::MismatchedEndTag { expected, found }) => {
            format!("element <{expected}> closed by </{found}>")
        }
        other => other.to_string(),
    };
    format!(
        "XML syntax error on line {}: {message}",
        line(xml, position)
    )
}

fn line(xml: &[u8], position: usize) -> usize {
    xml[..position.min(xml.len())]
        .iter()
        .filter(|byte| **byte == b'\n')
        .count()
        + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn soap_action_headers_parse_like_dms() {
        let action =
            parse_action("\"urn:schemas-upnp-org:service:ContentDirectory:1#Browse\"").unwrap();
        assert_eq!(action.kind, "ContentDirectory");
        assert_eq!(action.action, "Browse");
        assert_eq!(
            action.urn(),
            "urn:schemas-upnp-org:service:ContentDirectory:1"
        );
        assert_eq!(parse_action("").unwrap(), SoapAction::default());
        assert_eq!(parse_action("unquoted#x").unwrap(), SoapAction::default());
        assert_eq!(parse_action("\"urn:x:y#Browse\"").unwrap_err(), "urn:x:y");
        assert_eq!(
            parse_action("\"urn:a:service:B:010#C\"").unwrap().version,
            8
        );
    }

    #[test]
    fn the_envelope_body_is_taken_verbatim() {
        let xml = br#"<?xml version="1.0"?><s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body><u:Browse xmlns:u="urn:x"><ObjectID>0</ObjectID></u:Browse></s:Body></s:Envelope>"#;
        assert_eq!(
            envelope_body(xml).unwrap(),
            br#"<u:Browse xmlns:u="urn:x"><ObjectID>0</ObjectID></u:Browse>"#
        );
        // Observed from MatriX.145.
        assert_eq!(
            envelope_body(b"<nope").unwrap_err(),
            "XML syntax error on line 1: unexpected EOF"
        );
        assert_eq!(envelope_body(b"").unwrap_err(), "EOF");
        assert_eq!(
            envelope_body(b"<Other/>").unwrap_err(),
            "expected element type <Envelope> but have <Other>"
        );
    }

    #[test]
    fn browse_arguments_parse_like_encoding_xml() {
        let browse = Browse::parse(
            b"<u:Browse><ObjectID>%2FTR</ObjectID><BrowseFlag>BrowseMetadata</BrowseFlag><StartingIndex> 2 </StartingIndex><RequestedCount></RequestedCount></u:Browse>",
        )
        .unwrap();
        assert_eq!(
            browse,
            Browse {
                object_id: "%2FTR".into(),
                flag: "BrowseMetadata".into(),
                start: 2,
                count: 0
            }
        );
        assert_eq!(
            Browse::parse(b"<a><StartingIndex>x</StartingIndex></a>").unwrap_err(),
            "strconv.ParseInt: parsing \"x\": invalid syntax"
        );
        assert_eq!(Browse::parse(b"").unwrap_err(), "EOF");
        assert_eq!(
            Browse::parse(b"<a><ObjectID>a&amp;b</ObjectID></a>")
                .unwrap()
                .object_id,
            "a&b"
        );
    }

    #[test]
    fn faults_match_the_reference_layout() {
        assert_eq!(
            fault_xml(&UpnpError::new(701, "bad ObjectID relative")),
            "<Fault xmlns=\"http://schemas.xmlsoap.org/soap/envelope/\">\n  <faultcode>http://schemas.xmlsoap.org/soap/envelope/:Client</faultcode>\n  <faultstring>UPnPError</faultstring>\n  <detail>\n    <UPnPError xmlns=\"urn:schemas-upnp-org:control-1-0\">\n      <errorCode>701</errorCode>\n      <errorDescription>bad ObjectID relative</errorDescription>\n    </UPnPError>\n  </detail>\n</Fault>"
        );
    }
}
