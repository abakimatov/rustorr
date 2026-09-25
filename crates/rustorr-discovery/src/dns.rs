//! The DNS message subset an mDNS responder needs: queries in, PTR/SRV/TXT/A
//! records out, packed as `miekg/dns` packs them.

use std::{
    collections::HashMap,
    net::{Ipv4Addr, Ipv6Addr},
};

pub const TYPE_A: u16 = 1;
pub const TYPE_PTR: u16 = 12;
pub const TYPE_TXT: u16 = 16;
pub const TYPE_AAAA: u16 = 28;
pub const TYPE_SRV: u16 = 33;
pub const CLASS_IN: u16 = 1;
pub const CACHE_FLUSH: u16 = 1 << 15;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Data {
    Ptr(String),
    Srv { port: u16, target: String },
    Txt(Vec<String>),
    A(Ipv4Addr),
    Aaaa(Ipv6Addr),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub name: String,
    pub class: u16,
    pub ttl: u32,
    pub data: Data,
}

impl Record {
    pub fn kind(&self) -> u16 {
        match self.data {
            Data::Ptr(_) => TYPE_PTR,
            Data::Srv { .. } => TYPE_SRV,
            Data::Txt(_) => TYPE_TXT,
            Data::A(_) => TYPE_A,
            Data::Aaaa(_) => TYPE_AAAA,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    pub name: String,
    pub kind: u16,
    pub class: u16,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Message {
    pub id: u16,
    pub response: bool,
    pub opcode: u8,
    pub authoritative: bool,
    pub recursion_desired: bool,
    pub checking_disabled: bool,
    pub questions: Vec<Question>,
    pub answers: Vec<Record>,
    pub authority: Vec<Record>,
    pub additional: Vec<Record>,
}

impl Message {
    /// `Msg.Pack`; `compress` shares name suffixes as `Msg.Compress` does.
    pub fn pack(&self, compress: bool) -> Vec<u8> {
        let mut writer = Writer {
            out: Vec::with_capacity(512),
            names: compress.then(HashMap::new),
        };
        let mut flags: u16 = 0;
        if self.response {
            flags |= 1 << 15;
        }
        flags |= u16::from(self.opcode & 0x0f) << 11;
        if self.authoritative {
            flags |= 1 << 10;
        }
        if self.recursion_desired {
            flags |= 1 << 8;
        }
        if self.checking_disabled {
            flags |= 1 << 4;
        }
        for value in [
            self.id,
            flags,
            count(self.questions.len()),
            count(self.answers.len()),
            count(self.authority.len()),
            count(self.additional.len()),
        ] {
            writer.out.extend_from_slice(&value.to_be_bytes());
        }
        for question in &self.questions {
            writer.name(&question.name, true);
            writer.out.extend_from_slice(&question.kind.to_be_bytes());
            writer.out.extend_from_slice(&question.class.to_be_bytes());
        }
        for record in self
            .answers
            .iter()
            .chain(&self.authority)
            .chain(&self.additional)
        {
            writer.record(record);
        }
        writer.out
    }

    /// Parses a message; `None` for anything malformed.
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        let header = |index: usize| -> Option<u16> {
            Some(u16::from_be_bytes([
                *bytes.get(index)?,
                *bytes.get(index + 1)?,
            ]))
        };
        let flags = header(2)?;
        let mut message = Message {
            id: header(0)?,
            response: flags & (1 << 15) != 0,
            opcode: u8::try_from((flags >> 11) & 0x0f).ok()?,
            authoritative: flags & (1 << 10) != 0,
            recursion_desired: flags & (1 << 8) != 0,
            checking_disabled: flags & (1 << 4) != 0,
            ..Message::default()
        };
        let (questions, answers, authority, additional) =
            (header(4)?, header(6)?, header(8)?, header(10)?);
        let mut offset = 12;
        for _ in 0..questions {
            let (name, next) = read_name(bytes, offset)?;
            let kind = u16::from_be_bytes([*bytes.get(next)?, *bytes.get(next + 1)?]);
            let class = u16::from_be_bytes([*bytes.get(next + 2)?, *bytes.get(next + 3)?]);
            message.questions.push(Question { name, kind, class });
            offset = next + 4;
        }
        for (section, total) in [(0, answers), (1, authority), (2, additional)] {
            for _ in 0..total {
                let (record, next) = read_record(bytes, offset)?;
                offset = next;
                // Only records this responder understands are kept; the
                // counts still describe the packet.
                let list = match section {
                    0 => &mut message.answers,
                    1 => &mut message.authority,
                    _ => &mut message.additional,
                };
                list.push(record.unwrap_or(Record {
                    name: String::new(),
                    class: 0,
                    ttl: 0,
                    data: Data::Txt(Vec::new()),
                }));
            }
        }
        Some(message)
    }
}

fn count(length: usize) -> u16 {
    u16::try_from(length).unwrap_or(u16::MAX)
}

struct Writer {
    out: Vec<u8>,
    names: Option<HashMap<String, u16>>,
}

impl Writer {
    fn name(&mut self, name: &str, compressible: bool) {
        let labels: Vec<&str> = name
            .trim_end_matches('.')
            .split('.')
            .filter(|label| !label.is_empty())
            .collect();
        for index in 0..labels.len() {
            let suffix = labels[index..].join(".").to_ascii_lowercase();
            if compressible
                && let Some(names) = &self.names
                && let Some(&pointer) = names.get(&suffix)
            {
                self.out
                    .extend_from_slice(&(0xC000 | pointer).to_be_bytes());
                return;
            }
            if let Some(names) = &mut self.names
                && let Ok(position) = u16::try_from(self.out.len())
                && position < 0x3FFF
            {
                names.entry(suffix).or_insert(position);
            }
            let label = labels[index].as_bytes();
            let length = label.len().min(63);
            self.out
                .push(u8::try_from(length).expect("a label is at most 63 bytes"));
            self.out.extend_from_slice(&label[..length]);
        }
        self.out.push(0);
    }

    fn record(&mut self, record: &Record) {
        self.name(&record.name, true);
        self.out.extend_from_slice(&record.kind().to_be_bytes());
        self.out.extend_from_slice(&record.class.to_be_bytes());
        self.out.extend_from_slice(&record.ttl.to_be_bytes());
        let length_at = self.out.len();
        self.out.extend_from_slice(&[0, 0]);
        match &record.data {
            Data::Ptr(target) => self.name(target, true),
            Data::Srv { port, target } => {
                self.out.extend_from_slice(&[0, 0, 0, 0]);
                self.out.extend_from_slice(&port.to_be_bytes());
                // miekg/dns never compresses an SRV target.
                self.name(target, false);
            }
            Data::Txt(strings) => {
                for text in strings {
                    let bytes = text.as_bytes();
                    let length = bytes.len().min(255);
                    self.out.push(u8::try_from(length).expect("at most 255"));
                    self.out.extend_from_slice(&bytes[..length]);
                }
            }
            Data::A(ip) => self.out.extend_from_slice(&ip.octets()),
            Data::Aaaa(ip) => self.out.extend_from_slice(&ip.octets()),
        }
        let length = u16::try_from(self.out.len() - length_at - 2).unwrap_or(u16::MAX);
        self.out[length_at..length_at + 2].copy_from_slice(&length.to_be_bytes());
    }
}

fn read_name(bytes: &[u8], mut offset: usize) -> Option<(String, usize)> {
    let mut labels = Vec::new();
    let mut end = None;
    for _ in 0..128 {
        let length = *bytes.get(offset)?;
        if length & 0xC0 == 0xC0 {
            end.get_or_insert(offset + 2);
            offset = (usize::from(length & 0x3F) << 8) | usize::from(*bytes.get(offset + 1)?);
            continue;
        }
        offset += 1;
        if length == 0 {
            let mut name = labels.join(".");
            name.push('.');
            return Some((name, end.unwrap_or(offset)));
        }
        let label = bytes.get(offset..offset + usize::from(length))?;
        labels.push(escape_label(label));
        offset += usize::from(length);
    }
    None
}

/// A label as `miekg/dns` renders an unpacked name: special characters
/// backslash-escaped, bytes outside printable ASCII as `\DDD`. zeroconf
/// compares these strings with its own unescaped names, so an instance name
/// with a space never matches a question.
fn escape_label(label: &[u8]) -> String {
    let mut text = String::with_capacity(label.len());
    for &byte in label {
        match byte {
            b'.' | b' ' | b'\'' | b'@' | b';' | b'(' | b')' | b'"' | b'\\' => {
                text.push('\\');
                text.push(char::from(byte));
            }
            0x21..=0x7e => text.push(char::from(byte)),
            _ => text.push_str(&format!("\\{byte:03}")),
        }
    }
    text
}

fn read_record(bytes: &[u8], offset: usize) -> Option<(Option<Record>, usize)> {
    let (name, offset) = read_name(bytes, offset)?;
    let field = |at: usize| -> Option<u16> {
        Some(u16::from_be_bytes([*bytes.get(at)?, *bytes.get(at + 1)?]))
    };
    let kind = field(offset)?;
    let class = field(offset + 2)?;
    let ttl = (u32::from(field(offset + 4)?) << 16) | u32::from(field(offset + 6)?);
    let length = usize::from(field(offset + 8)?);
    let start = offset + 10;
    let end = start + length;
    bytes.get(start..end)?;
    let data = match kind {
        TYPE_PTR => read_name(bytes, start).map(|(target, _)| Data::Ptr(target)),
        _ => None,
    };
    Some((
        data.map(|data| Record {
            name,
            class,
            ttl,
            data,
        }),
        end,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_round_trip_with_compressed_names() {
        let message = Message {
            response: true,
            authoritative: true,
            answers: vec![Record {
                name: "_torrserver._tcp.local.".into(),
                class: CLASS_IN,
                ttl: 3200,
                data: Data::Ptr("TorrServer._torrserver._tcp.local.".into()),
            }],
            additional: vec![Record {
                name: "TorrServer._torrserver._tcp.local.".into(),
                class: CLASS_IN | CACHE_FLUSH,
                ttl: 120,
                data: Data::Srv {
                    port: 8090,
                    target: "host.local.".into(),
                },
            }],
            ..Message::default()
        };
        let packed = message.pack(true);
        let parsed = Message::parse(&packed).unwrap();
        assert!(parsed.response && parsed.authoritative);
        assert_eq!(parsed.answers, message.answers);
        assert_eq!(parsed.additional.len(), 1);
        // The PTR target reuses the owner name's suffix.
        assert!(packed.len() < message.pack(false).len());
    }

    #[test]
    fn parsed_names_are_escaped_like_miekg() {
        let query = Message {
            questions: vec![Question {
                name: "Contract DLNA._torrserver._tcp.local.".into(),
                kind: TYPE_SRV,
                class: CLASS_IN,
            }],
            ..Message::default()
        };
        let parsed = Message::parse(&query.pack(false)).unwrap();
        assert_eq!(
            parsed.questions[0].name,
            "Contract\\ DLNA._torrserver._tcp.local."
        );
        assert_eq!(escape_label("é".as_bytes()), "\\195\\169");
    }

    #[test]
    fn questions_parse_with_their_class_bits() {
        let query = Message {
            id: 7,
            questions: vec![Question {
                name: "_http._tcp.local.".into(),
                kind: TYPE_PTR,
                class: CLASS_IN | CACHE_FLUSH,
            }],
            ..Message::default()
        };
        let parsed = Message::parse(&query.pack(false)).unwrap();
        assert_eq!(parsed.id, 7);
        assert_eq!(parsed.questions, query.questions);
        assert!(Message::parse(&[0, 1, 2]).is_none());
    }
}
