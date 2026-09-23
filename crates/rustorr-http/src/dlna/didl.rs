//! UPnP AV objects written as Go's `encoding/xml` marshals the
//! `anacrolix/dms/upnpav` types.

/// `xml.EscapeText`: the five markup characters and the three whitespace
/// controls as character references; characters XML cannot carry become
/// U+FFFD.
pub(crate) fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '"' => escaped.push_str("&#34;"),
            '\'' => escaped.push_str("&#39;"),
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '\t' => escaped.push_str("&#x9;"),
            '\n' => escaped.push_str("&#xA;"),
            '\r' => escaped.push_str("&#xD;"),
            character if allowed(character) => escaped.push(character),
            _ => escaped.push('\u{FFFD}'),
        }
    }
    escaped
}

/// `isInCharacterRange`.
fn allowed(character: char) -> bool {
    matches!(character,
        '\u{09}' | '\u{0A}' | '\u{0D}'
        | '\u{20}'..='\u{D7FF}'
        | '\u{E000}'..='\u{FFFD}'
        | '\u{10000}'..='\u{10FFFF}')
}

/// The fields of `upnpav.Object` TorrServer fills in.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Object {
    pub id: String,
    pub parent_id: String,
    pub restricted: u8,
    pub searchable: u8,
    pub title: String,
    pub class: String,
    pub icon: String,
    /// `YYYY-MM-DD`, as `upnpav.Timestamp` writes it.
    pub date: String,
    pub album_art_uri: String,
}

impl Object {
    /// Attributes, then elements, in struct field order.
    fn write(&self, tag: &str, extra_attributes: &str, children: &str) -> String {
        let mut xml = format!(
            "<{tag} id=\"{}\" parentID=\"{}\" restricted=\"{}\" searchable=\"{}\"{extra_attributes}>",
            escape(&self.id),
            escape(&self.parent_id),
            self.restricted,
            self.searchable
        );
        xml.push_str(&format!("<dc:title>{}</dc:title>", escape(&self.title)));
        xml.push_str(&format!("<upnp:class>{}</upnp:class>", escape(&self.class)));
        if !self.icon.is_empty() {
            xml.push_str(&format!("<upnp:icon>{}</upnp:icon>", escape(&self.icon)));
        }
        xml.push_str(&format!("<dc:date>{}</dc:date>", escape(&self.date)));
        if !self.album_art_uri.is_empty() {
            xml.push_str(&format!(
                "<upnp:albumArtURI>{}</upnp:albumArtURI>",
                escape(&self.album_art_uri)
            ));
        }
        xml.push_str(children);
        xml.push_str(&format!("</{tag}>"));
        xml
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Resource {
    pub protocol_info: String,
    pub url: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Entry {
    Container {
        object: Object,
        child_count: usize,
    },
    Item {
        object: Object,
        resources: Vec<Resource>,
    },
}

impl Entry {
    pub(crate) fn container(object: Object, child_count: usize) -> Self {
        Self::Container {
            object,
            child_count,
        }
    }

    pub(crate) fn xml(&self) -> String {
        match self {
            Self::Container {
                object,
                child_count,
            } => object.write("container", &format!(" childCount=\"{child_count}\""), ""),
            Self::Item { object, resources } => {
                let children: String = resources
                    .iter()
                    .map(|resource| {
                        let size = if resource.size == 0 {
                            String::new()
                        } else {
                            format!(" size=\"{}\"", resource.size)
                        };
                        format!(
                            "<res protocolInfo=\"{}\"{size}>{}</res>",
                            escape(&resource.protocol_info),
                            escape(&resource.url)
                        )
                    })
                    .collect();
                object.write("item", "", &children)
            }
        }
    }
}

/// `didl_lite`: the entries inside a DIDL-Lite document.
pub(crate) fn didl_lite(entries: &str) -> String {
    format!(
        "<DIDL-Lite xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:upnp=\"urn:schemas-upnp-org:metadata-1-0/upnp/\" xmlns=\"urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/\" xmlns:dlna=\"urn:schemas-dlna-org:metadata-1-0/\">{entries}</DIDL-Lite>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escaping_follows_go() {
        assert_eq!(
            escape("a\"b'<&>\t\n\r"),
            "a&#34;b&#39;&lt;&amp;&gt;&#x9;&#xA;&#xD;"
        );
        assert_eq!(escape("\u{1}"), "\u{FFFD}");
    }

    #[test]
    fn containers_put_attributes_first_and_skip_empty_optional_elements() {
        let entry = Entry::container(
            Object {
                id: "%2FTR".into(),
                parent_id: "0".into(),
                restricted: 1,
                title: "Torrents".into(),
                class: "object.container.storageFolder".into(),
                date: "2026-09-23".into(),
                ..Object::default()
            },
            2,
        );
        assert_eq!(
            entry.xml(),
            "<container id=\"%2FTR\" parentID=\"0\" restricted=\"1\" searchable=\"0\" childCount=\"2\">\
             <dc:title>Torrents</dc:title><upnp:class>object.container.storageFolder</upnp:class>\
             <dc:date>2026-09-23</dc:date></container>"
        );
    }
}
