//! WebDAV request bodies read and responses written as `x/net/webdav`'s
//! internal `encoding/xml` fork does.

use std::ops::Range;

use quick_xml::{
    NsReader,
    events::Event,
    name::{Namespace, ResolveResult},
};

use crate::dlna::escape_text;

pub(crate) const DAV: &str = "DAV:";

/// A property name: namespace and local name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct Name {
    pub space: String,
    pub local: String,
}

impl Name {
    pub(crate) fn dav(local: &str) -> Self {
        Self {
            space: DAV.into(),
            local: local.into(),
        }
    }

    /// The element as `propstat.MarshalXML` writes it: `D:` for `DAV:`
    /// properties, a default namespace declaration for any other.
    pub(crate) fn element(&self, inner: &str) -> String {
        if self.space == DAV {
            format!("<D:{0}>{inner}</D:{0}>", self.local)
        } else if self.space.is_empty() {
            format!("<{0}>{inner}</{0}>", self.local)
        } else {
            format!(
                "<{0} xmlns=\"{1}\">{inner}</{0}>",
                self.local,
                escape_text(&self.space)
            )
        }
    }
}

/// A parsed `PROPFIND` body.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Propfind {
    pub allprop: bool,
    pub propname: bool,
    pub prop: Option<Vec<Name>>,
    pub include: Option<Vec<Name>>,
}

fn resolved(namespace: &ResolveResult<'_>) -> String {
    match namespace {
        ResolveResult::Bound(Namespace(space)) => String::from_utf8_lossy(space).into_owned(),
        _ => String::new(),
    }
}

/// An element of a request body, with namespaces resolved in context.
#[derive(Debug)]
struct Element {
    name: Name,
    children: Vec<Element>,
    /// Character data directly inside, whitespace included.
    text: String,
    /// The raw bytes between the start and end tags.
    content: Range<usize>,
}

/// The first element of a body, read as Go's decoder reads one value:
/// `Ok(None)` without any element, `Err` for malformed XML.
fn document(body: &[u8]) -> Result<Option<Element>, ()> {
    let mut reader = NsReader::from_reader(body);
    reader.config_mut().check_end_names = true;
    let mut stack: Vec<Element> = Vec::new();
    loop {
        let before = usize::try_from(reader.buffer_position()).map_err(|_| ())?;
        let (space, event) = {
            let (namespace, event) = reader.read_resolved_event().map_err(|_| ())?;
            (resolved(&namespace), event)
        };
        let after = usize::try_from(reader.buffer_position()).map_err(|_| ())?;
        let element = |local: &[u8]| Element {
            name: Name {
                space: space.clone(),
                local: String::from_utf8_lossy(local).into_owned(),
            },
            children: Vec::new(),
            text: String::new(),
            content: after..after,
        };
        let finished = match event {
            Event::Start(start) => {
                stack.push(element(start.local_name().as_ref()));
                None
            }
            Event::Empty(start) => Some(element(start.local_name().as_ref())),
            Event::End(_) => {
                let mut done = stack.pop().ok_or(())?;
                done.content = done.content.start..before;
                Some(done)
            }
            Event::Text(text) => {
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&String::from_utf8_lossy(&text));
                }
                None
            }
            Event::CData(text) => {
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&String::from_utf8_lossy(&text));
                }
                None
            }
            Event::GeneralRef(reference) => {
                if let Some(top) = stack.last_mut() {
                    top.text.push('&');
                    top.text.push_str(&String::from_utf8_lossy(&reference));
                    top.text.push(';');
                }
                None
            }
            Event::Eof => {
                return if stack.is_empty() { Ok(None) } else { Err(()) };
            }
            _ => None,
        };
        if let Some(done) = finished {
            match stack.last_mut() {
                Some(parent) => parent.children.push(done),
                None => return Ok(Some(done)),
            }
        }
    }
}

/// `propfindProps.UnmarshalXML`: each child must close right after it
/// opens, and there must be at least one.
fn property_names(element: &Element) -> Result<Vec<Name>, ()> {
    if element.children.is_empty() {
        return Err(());
    }
    element
        .children
        .iter()
        .map(|child| {
            if child.children.is_empty() && child.text.is_empty() {
                Ok(child.name.clone())
            } else {
                Err(())
            }
        })
        .collect()
}

/// `readPropfind`: an empty body is `allprop`; anything malformed is `Err`.
pub(crate) fn propfind(body: &[u8]) -> Result<Propfind, ()> {
    if body.is_empty() {
        return Ok(Propfind {
            allprop: true,
            ..Propfind::default()
        });
    }
    let root = document(body)?.ok_or(())?;
    if root.name != Name::dav("propfind") {
        return Err(());
    }
    let mut found = Propfind::default();
    for child in &root.children {
        if child.name.space != DAV {
            continue;
        }
        match child.name.local.as_str() {
            "allprop" => found.allprop = true,
            "propname" => found.propname = true,
            "prop" => found
                .prop
                .get_or_insert_with(Vec::new)
                .extend(property_names(child)?),
            "include" => found
                .include
                .get_or_insert_with(Vec::new)
                .extend(property_names(child)?),
            _ => {}
        }
    }
    let invalid = (!found.allprop && found.include.is_some())
        || (found.allprop && (found.prop.is_some() || found.propname))
        || (found.prop.is_some() && found.propname)
        || (!found.propname && !found.allprop && found.prop.is_none());
    if invalid { Err(()) } else { Ok(found) }
}

/// `readProppatch`: the property names each `set` or `remove` touches.
pub(crate) fn proppatch(body: &[u8]) -> Result<Vec<Name>, ()> {
    let root = document(body)?.ok_or(())?;
    if root.name != Name::dav("propertyupdate") {
        return Err(());
    }
    let mut names = Vec::new();
    for operation in &root.children {
        let remove = match (operation.name.space.as_str(), operation.name.local.as_str()) {
            (DAV, "set") => false,
            (DAV, "remove") => true,
            _ => return Err(()),
        };
        for prop in operation
            .children
            .iter()
            .filter(|child| child.name == Name::dav("prop"))
        {
            if prop.children.is_empty() {
                return Err(());
            }
            for property in &prop.children {
                if remove && !body[property.content.clone()].is_empty() {
                    return Err(());
                }
                names.push(property.name.clone());
            }
        }
    }
    Ok(names)
}

/// A `LOCK` body's request.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum LockInfo {
    /// No body: a refresh.
    Empty,
    Exclusive {
        owner_xml: String,
    },
    /// Any other scope or type, which `x/net/webdav` does not implement.
    Unsupported,
}

/// `readLockInfo`: element names match by local name, as the struct tags
/// carry no namespace.
pub(crate) fn lockinfo(body: &[u8]) -> Result<LockInfo, ()> {
    if body.is_empty() {
        return Ok(LockInfo::Empty);
    }
    let root = document(body)?.ok_or(())?;
    if root.name.local != "lockinfo" {
        return Err(());
    }
    let (mut exclusive, mut shared, mut write, mut owner) = (false, false, false, String::new());
    for child in &root.children {
        match child.name.local.as_str() {
            "lockscope" | "locktype" => {
                for item in &child.children {
                    match (child.name.local.as_str(), item.name.local.as_str()) {
                        ("lockscope", "exclusive") => exclusive = true,
                        ("lockscope", "shared") => shared = true,
                        ("locktype", "write") => write = true,
                        _ => {}
                    }
                }
            }
            "owner" => owner = String::from_utf8_lossy(&body[child.content.clone()]).into_owned(),
            _ => {}
        }
    }
    if !exclusive || shared || !write {
        return Ok(LockInfo::Unsupported);
    }
    Ok(LockInfo::Exclusive { owner_xml: owner })
}

/// `escape` in `writeLockInfo`: text escaped only when it has markup.
fn escape_markup(text: &str) -> String {
    if text.contains(['"', '&', '\'', '<', '>']) {
        escape_text(text)
    } else {
        text.into()
    }
}

/// `writeLockInfo`.
pub(crate) fn lock_body(
    token: &str,
    root: &str,
    owner_xml: &str,
    timeout: Option<u64>,
    zero_depth: bool,
) -> String {
    let depth = if zero_depth { "0" } else { "infinity" };
    // An infinite duration is -1 ns, which Go divides to 0 seconds.
    let timeout = timeout.unwrap_or(0);
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<D:prop xmlns:D=\"DAV:\"><D:lockdiscovery><D:activelock>\n\
         \t<D:locktype><D:write/></D:locktype>\n\
         \t<D:lockscope><D:exclusive/></D:lockscope>\n\
         \t<D:depth>{depth}</D:depth>\n\
         \t<D:owner>{owner_xml}</D:owner>\n\
         \t<D:timeout>Second-{timeout}</D:timeout>\n\
         \t<D:locktoken><D:href>{}</D:href></D:locktoken>\n\
         \t<D:lockroot><D:href>{}</D:href></D:lockroot>\n\
         </D:activelock></D:lockdiscovery></D:prop>",
        escape_markup(token),
        escape_markup(root)
    )
}

/// One property with its value, and the status of a propstat group.
pub(crate) struct Propstat {
    pub props: Vec<(Name, String)>,
    pub status: u16,
    pub error: Option<&'static str>,
}

/// A `<D:response>` element.
pub(crate) fn response(href: &str, propstats: &[Propstat]) -> String {
    let mut xml = format!("<D:response><D:href>{}</D:href>", escape_text(href));
    for propstat in propstats {
        xml.push_str("<D:propstat><D:prop>");
        for (name, inner) in &propstat.props {
            xml.push_str(&name.element(inner));
        }
        xml.push_str(&format!(
            "</D:prop><D:status>HTTP/1.1 {} {}</D:status>",
            propstat.status,
            super::status_text(propstat.status)
        ));
        if let Some(error) = propstat.error {
            xml.push_str(&format!("<D:error>{error}</D:error>"));
        }
        xml.push_str("</D:propstat>");
    }
    xml.push_str("</D:response>");
    xml
}

/// The multistatus document around its responses.
pub(crate) fn multistatus(responses: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><D:multistatus xmlns:D=\"DAV:\">{responses}</D:multistatus>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn propfind_bodies_follow_the_reference_rules() {
        assert!(propfind(b"").unwrap().allprop);
        let named = propfind(br#"<D:propfind xmlns:D="DAV:" xmlns:x="urn:x"><D:prop><D:displayname/><x:custom></x:custom></D:prop></D:propfind>"#).unwrap();
        assert_eq!(
            named.prop.unwrap(),
            vec![
                Name::dav("displayname"),
                Name {
                    space: "urn:x".into(),
                    local: "custom".into()
                }
            ]
        );
        assert!(
            propfind(br#"<propfind xmlns="DAV:"><propname/></propfind>"#)
                .unwrap()
                .propname
        );
        for invalid in [
            b"<nope".as_slice(),
            b" ",
            br#"<D:propfind xmlns:D="DAV:"><D:allprop/><D:propname/></D:propfind>"#,
            br#"<D:propfind xmlns:D="DAV:"><D:prop/></D:propfind>"#,
            br#"<D:propfind xmlns:D="DAV:"><D:prop><D:a>x</D:a></D:prop></D:propfind>"#,
            br#"<propfind/>"#,
            br#"<D:propfind xmlns:D="DAV:"/>"#,
        ] {
            assert!(
                propfind(invalid).is_err(),
                "{}",
                String::from_utf8_lossy(invalid)
            );
        }
    }

    #[test]
    fn proppatch_and_lockinfo_parse() {
        let names = proppatch(br#"<D:propertyupdate xmlns:D="DAV:" xmlns:x="urn:x"><D:set><D:prop><x:note>x</x:note></D:prop></D:set></D:propertyupdate>"#).unwrap();
        assert_eq!(names[0].local, "note");
        assert!(
            proppatch(br#"<D:propertyupdate xmlns:D="DAV:"><D:other/></D:propertyupdate>"#)
                .is_err()
        );
        assert_eq!(lockinfo(b"").unwrap(), LockInfo::Empty);
        assert_eq!(
            lockinfo(br#"<D:lockinfo xmlns:D="DAV:"><D:lockscope><D:exclusive/></D:lockscope><D:locktype><D:write/></D:locktype><D:owner><D:href>me</D:href></D:owner></D:lockinfo>"#).unwrap(),
            LockInfo::Exclusive { owner_xml: "<D:href>me</D:href>".into() }
        );
        assert_eq!(
            lockinfo(br#"<D:lockinfo xmlns:D="DAV:"><D:lockscope><D:shared/></D:lockscope><D:locktype><D:write/></D:locktype></D:lockinfo>"#).unwrap(),
            LockInfo::Unsupported
        );
    }

    #[test]
    fn property_elements_match_the_encoder() {
        assert_eq!(
            Name::dav("displayname").element("x"),
            "<D:displayname>x</D:displayname>"
        );
        assert_eq!(
            Name {
                space: "urn:x".into(),
                local: "custom".into()
            }
            .element(""),
            "<custom xmlns=\"urn:x\"></custom>"
        );
    }
}
