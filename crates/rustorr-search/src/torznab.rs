//! Torznab indexers as MatriX.145 queries them.

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use quick_xml::{Reader, XmlVersion, events::Event};

use crate::{TorrentDetails, service::REQUEST_TIMEOUT};

/// Go's `url.QueryEscape` keeps only these besides ASCII letters and digits.
const QUERY: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CategoryType {
    /// Movies and TV (`cat=5000,2000`); also any unrecognised value.
    Default,
    /// The indexer's own `Categories`, when set.
    Manual,
    /// No category filter.
    All,
}

impl CategoryType {
    pub fn parse(value: &str) -> Self {
        match value {
            "manual" => Self::Manual,
            "all" => Self::All,
            _ => Self::Default,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Indexer {
    pub host: String,
    pub key: String,
    pub categories: String,
    pub category_type: CategoryType,
}

fn escape(value: &str) -> String {
    utf8_percent_encode(value, QUERY)
        .to_string()
        .replace("%20", "+")
}

fn with_slash(host: &str) -> String {
    if host.ends_with('/') {
        host.to_owned()
    } else {
        format!("{host}/")
    }
}

/// `url.Values.Encode()` of the search request: keys in sorted order.
pub(crate) fn search_url(indexer: &Indexer, query: &str) -> String {
    let category = match indexer.category_type {
        CategoryType::All => None,
        CategoryType::Manual => {
            (!indexer.categories.is_empty()).then(|| indexer.categories.clone())
        }
        CategoryType::Default => Some("5000,2000".to_owned()),
    };
    let mut parameters = vec![format!("apikey={}", escape(&indexer.key))];
    if let Some(category) = category {
        parameters.push(format!("cat={}", escape(&category)));
    }
    parameters.push(format!("q={}", escape(query)));
    parameters.push("t=search".to_owned());
    format!("{}api?{}", with_slash(&indexer.host), parameters.join("&"))
}

pub(crate) async fn search_one(
    client: &reqwest::Client,
    indexer: &Indexer,
    query: &str,
) -> Option<Vec<TorrentDetails>> {
    let response = client
        .get(search_url(indexer, query))
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await
        .ok()?;
    if response.status() != reqwest::StatusCode::OK {
        return None;
    }
    parse_items(&response.bytes().await.ok()?)
}

/// `torznab.Test`: `Ok` when the indexer answers `t=caps` with `<caps>`.
pub(crate) async fn test(client: &reqwest::Client, host: &str, key: &str) -> Result<(), String> {
    let host = if host.starts_with("http://") || host.starts_with("https://") {
        host.to_owned()
    } else {
        format!("http://{host}")
    };
    let url = format!("{}api?apikey={}&t=caps", with_slash(&host), escape(key));
    let response = client
        .get(&url)
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await
        .map_err(|error| format!("Get \"{url}\": {error}"))?;
    let status = response.status();
    if status != reqwest::StatusCode::OK {
        return Err(format!(
            "status: {} {}",
            status.as_u16(),
            status.canonical_reason().unwrap_or_default()
        ));
    }
    let body = response.bytes().await.map_err(|error| error.to_string())?;
    let (root, attributes) =
        root_element(&body).map_err(|error| format!("invalid xml response: {error}"))?;
    match root.as_str() {
        "error" => {
            let description = attributes.get("description").cloned().unwrap_or_default();
            let message = if description.is_empty() {
                attributes.get("code").cloned().unwrap_or_default()
            } else {
                description
            };
            Err(format!("api error: {message}"))
        }
        "caps" => Ok(()),
        other => Err(format!("unexpected xml root: {other}")),
    }
}

type Attributes = std::collections::HashMap<String, String>;

fn attributes(element: &quick_xml::events::BytesStart<'_>) -> Attributes {
    element
        .attributes()
        .filter_map(Result::ok)
        .filter_map(|attribute| {
            let name = String::from_utf8_lossy(attribute.key.local_name().as_ref()).into_owned();
            let value = attribute
                .normalized_value(XmlVersion::Implicit1_0)
                .ok()?
                .into_owned();
            Some((name, value))
        })
        .collect()
}

fn local_name(element: &quick_xml::events::BytesStart<'_>) -> String {
    String::from_utf8_lossy(element.local_name().as_ref()).into_owned()
}

fn root_element(body: &[u8]) -> Result<(String, Attributes), String> {
    let mut reader = Reader::from_reader(body);
    loop {
        match reader.read_event().map_err(|error| error.to_string())? {
            Event::Start(element) | Event::Empty(element) => {
                return Ok((local_name(&element), attributes(&element)));
            }
            Event::Eof => return Err("EOF".into()),
            _ => {}
        }
    }
}

#[derive(Default)]
struct Item {
    title: String,
    link: String,
    pub_date: String,
    size: String,
    indexer: String,
    prowlarr: String,
    enclosures: Vec<Attributes>,
    attributes: Vec<Attributes>,
}

/// The items of `<rss><channel><item>…`. Like Go's decoder, a malformed
/// document or number rejects the whole response.
pub(crate) fn parse_items(body: &[u8]) -> Option<Vec<TorrentDetails>> {
    let mut reader = Reader::from_reader(body);
    let mut path: Vec<String> = Vec::new();
    let mut items = Vec::new();
    let mut item: Option<Item> = None;
    let mut text = String::new();
    loop {
        match reader.read_event().ok()? {
            Event::Start(element) => {
                let name = local_name(&element);
                if path.len() == 2 && path[1] == "channel" && name == "item" {
                    item = Some(Item::default());
                } else if path.len() == 3 && item.is_some() {
                    record_empty(item.as_mut()?, &name, &element);
                }
                path.push(name);
                text.clear();
            }
            Event::Empty(element) => {
                if path.len() == 3
                    && let Some(item) = item.as_mut()
                {
                    record_empty(item, &local_name(&element), &element);
                }
            }
            Event::Text(content) => {
                text.push_str(&content.xml_content(XmlVersion::Implicit1_0).ok()?)
            }
            Event::CData(content) => text.push_str(&String::from_utf8_lossy(&content)),
            Event::GeneralRef(reference) => {
                if let Ok(Some(character)) = reference.resolve_char_ref() {
                    text.push(character);
                } else {
                    let name = String::from_utf8_lossy(&reference);
                    text.push_str(match name.as_ref() {
                        "amp" => "&",
                        "lt" => "<",
                        "gt" => ">",
                        "quot" => "\"",
                        "apos" => "'",
                        _ => "",
                    });
                }
            }
            Event::End(_) => {
                let name = path.pop().unwrap_or_default();
                if path.len() == 3
                    && let Some(item) = item.as_mut()
                {
                    let value = std::mem::take(&mut text);
                    match name.as_str() {
                        "title" => item.title = value,
                        "link" => item.link = value,
                        "pubDate" => item.pub_date = value,
                        "size" => item.size = value,
                        "jackettindexer" => item.indexer = value,
                        "prowlarrindexer" => item.prowlarr = value,
                        _ => {}
                    }
                } else if path.len() == 2 && name == "item" {
                    items.push(details(item.take()?)?);
                }
                text.clear();
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Some(items)
}

fn record_empty(item: &mut Item, name: &str, element: &quick_xml::events::BytesStart<'_>) {
    match name {
        "enclosure" => item.enclosures.push(attributes(element)),
        "attr" => item.attributes.push(attributes(element)),
        _ => {}
    }
}

fn go_int(text: &str) -> Option<i64> {
    let text = text.trim();
    if text.is_empty() {
        Some(0)
    } else {
        text.parse().ok()
    }
}

fn details(item: Item) -> Option<TorrentDetails> {
    let size = go_int(&item.size)?;
    let mut details = TorrentDetails {
        title: item.title.clone(),
        name: item.title,
        link: item.link,
        tracker: if item.indexer.is_empty() {
            item.prowlarr
        } else {
            item.indexer
        },
        create_date: go_date(&item.pub_date),
        ..TorrentDetails::default()
    };
    if let Some(enclosure) = item.enclosures.first() {
        details.link = enclosure.get("url").cloned().unwrap_or_default();
        details.size = format_size(go_int(enclosure.get("length").map_or("", String::as_str))?);
    } else {
        details.size = format_size(size);
    }
    for attribute in &item.attributes {
        let value = attribute.get("value").map_or("", String::as_str);
        match attribute.get("name").map(String::as_str) {
            Some("magneturl") => {
                details.magnet = value.to_owned();
                details.hash = extract_hash(value);
            }
            Some("seeders") => details.seed = value.parse().unwrap_or(0),
            Some("peers") => details.peer = value.parse().unwrap_or(0),
            _ => {}
        }
    }
    if details.magnet.is_empty() && details.link.starts_with("magnet:") {
        details.magnet = details.link.clone();
        details.hash = extract_hash(&details.magnet);
    }
    Some(details)
}

/// `formatSize`, including the reference's `KCiB`/`MCiB`/`GCiB` units.
pub(crate) fn format_size(bytes: i64) -> String {
    const UNIT: i64 = 1024;
    if bytes < UNIT {
        return format!("{bytes} B");
    }
    let (mut divisor, mut exponent) = (UNIT, 0);
    let mut remaining = bytes / UNIT;
    while remaining >= UNIT {
        divisor *= UNIT;
        exponent += 1;
        remaining /= UNIT;
    }
    format!(
        "{:.1} {}CiB",
        bytes as f64 / divisor as f64,
        "KMGTPE".as_bytes()[exponent] as char
    )
}

fn extract_hash(magnet: &str) -> String {
    let Some(query) = magnet.strip_prefix("magnet:?") else {
        return String::new();
    };
    query
        .split('&')
        .find_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            (key == "xt").then(|| {
                percent_encoding::percent_decode_str(&value.replace('+', " "))
                    .decode_utf8_lossy()
                    .into_owned()
            })
        })
        .and_then(|xt| xt.strip_prefix("urn:btih:").map(str::to_owned))
        .unwrap_or_default()
}

/// RFC 1123 dates as Go parses them and writes them back as RFC 3339; a
/// zero offset becomes `Z`. The reference uses the current time for dates
/// it cannot parse.
pub(crate) fn go_date(text: &str) -> String {
    parse_rfc1123(text).unwrap_or_else(|| {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        format_rfc3339(i64::try_from(now).unwrap_or(0), 0)
    })
}

fn parse_rfc1123(text: &str) -> Option<String> {
    // "Mon, 02 Jan 2006 15:04:05 MST" or "... -0700".
    let rest = text.split_once(", ")?.1;
    let fields: Vec<&str> = rest.split(' ').collect();
    let [day, month, year, time, zone] = fields.as_slice() else {
        return None;
    };
    if day.len() != 2 || year.len() != 4 {
        return None;
    }
    let month = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ]
    .iter()
    .position(|name| name == month)? as i64
        + 1;
    let (day, year): (i64, i64) = (day.parse().ok()?, year.parse().ok()?);
    let clock: Vec<i64> = time
        .split(':')
        .map(|part| part.parse().ok())
        .collect::<Option<_>>()?;
    let [hour, minute, second] = clock.as_slice() else {
        return None;
    };
    let offset = if let Some(sign) = zone.chars().next().filter(|c| *c == '+' || *c == '-') {
        let digits = &zone[1..];
        if digits.len() != 4 {
            return None;
        }
        let value = (digits[..2].parse::<i64>().ok()? * 60 + digits[2..].parse::<i64>().ok()?) * 60;
        if sign == '-' { -value } else { value }
    } else if zone.chars().all(|c| c.is_ascii_uppercase()) && (3..=5).contains(&zone.len()) {
        0
    } else {
        return None;
    };
    let local = days(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second;
    Some(format_rfc3339(local - offset, offset))
}

fn days(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    era * 146_097 + year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year - 719_468
}

fn format_rfc3339(utc_seconds: i64, offset: i64) -> String {
    let local = utc_seconds + offset;
    let (day_number, seconds) = (local.div_euclid(86_400), local.rem_euclid(86_400));
    let (year, month, day) = civil(day_number);
    let zone = if offset == 0 {
        "Z".to_owned()
    } else {
        let sign = if offset < 0 { '-' } else { '+' };
        format!(
            "{sign}{:02}:{:02}",
            offset.abs() / 3600,
            offset.abs() % 3600 / 60
        )
    };
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}{zone}",
        seconds / 3600,
        seconds % 3600 / 60,
        seconds % 60
    )
}

/// Howard Hinnant's civil-from-days.
fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    (year_of_era + era * 400 + i64::from(month <= 2), month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn indexer(category_type: CategoryType, categories: &str) -> Indexer {
        Indexer {
            host: "http://indexer:9117".into(),
            key: "fixture key".into(),
            categories: categories.into(),
            category_type,
        }
    }

    #[test]
    fn search_urls_encode_parameters_in_key_order() {
        assert_eq!(
            search_url(&indexer(CategoryType::Default, ""), "Series 2020"),
            "http://indexer:9117/api?apikey=fixture+key&cat=5000%2C2000&q=Series+2020&t=search"
        );
        assert_eq!(
            search_url(&indexer(CategoryType::Manual, "2000"), "x"),
            "http://indexer:9117/api?apikey=fixture+key&cat=2000&q=x&t=search"
        );
        assert_eq!(
            search_url(&indexer(CategoryType::Manual, ""), "x"),
            "http://indexer:9117/api?apikey=fixture+key&q=x&t=search"
        );
        assert_eq!(
            search_url(&indexer(CategoryType::All, "2000"), "x"),
            "http://indexer:9117/api?apikey=fixture+key&q=x&t=search"
        );
    }

    #[test]
    fn items_map_to_details_like_the_reference() {
        let body = br#"<?xml version="1.0"?><rss xmlns:torznab="http://torznab.com/schemas/2015/feed"><channel>
            <item><title>Film &amp; Co</title><link>http://i/1</link><pubDate>Sat, 01 May 2021 12:00:00 +0000</pubDate>
              <size>1</size><jackettindexer>Fake</jackettindexer>
              <enclosure url="http://i/d/1" length="4692251852" type="application/x-bittorrent"/>
              <torznab:attr name="seeders" value="42"/><torznab:attr name="peers" value="45"/>
              <torznab:attr name="magneturl" value="magnet:?xt=urn:btih:ABCD&amp;dn=x"/></item>
            <item><title>Other</title><link>magnet:?xt=urn%3Abtih%3Aef01</link>
              <pubDate>Fri, 20 Nov 2020 08:30:00 +0300</pubDate><size>2048</size>
              <prowlarrindexer>Prowlarr</prowlarrindexer></item>
          </channel></rss>"#;
        let items = parse_items(body).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].title, "Film & Co");
        assert_eq!(items[0].name, "Film & Co");
        assert_eq!(items[0].link, "http://i/d/1");
        assert_eq!(items[0].size, "4.4 GCiB");
        assert_eq!(items[0].create_date, "2021-05-01T12:00:00Z");
        assert_eq!((items[0].seed, items[0].peer), (42, 45));
        assert_eq!(items[0].magnet, "magnet:?xt=urn:btih:ABCD&dn=x");
        assert_eq!(items[0].hash, "ABCD");
        assert_eq!(items[0].names, None);
        assert_eq!(items[1].tracker, "Prowlarr");
        assert_eq!(items[1].size, "2.0 KCiB");
        assert_eq!(items[1].create_date, "2020-11-20T08:30:00+03:00");
        assert_eq!(items[1].magnet, "magnet:?xt=urn%3Abtih%3Aef01");
        assert_eq!(items[1].hash, "ef01");
    }

    #[test]
    fn a_bad_number_rejects_the_whole_response() {
        let body = b"<rss><channel><item><size>big</size></item></channel></rss>";
        assert!(parse_items(body).is_none());
        assert!(
            parse_items(b"<rss><channel></channel></rss>")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn sizes_use_the_reference_units() {
        assert_eq!(format_size(1023), "1023 B");
        assert_eq!(format_size(1536), "1.5 KCiB");
        assert_eq!(format_size(12_992_276_480), "12.1 GCiB");
    }

    #[test]
    fn dates_keep_their_offset_and_zero_is_z() {
        assert_eq!(
            go_date("Sat, 01 May 2021 12:00:00 GMT"),
            "2021-05-01T12:00:00Z"
        );
        assert_eq!(
            go_date("Sat, 01 May 2021 12:00:00 -0130"),
            "2021-05-01T12:00:00-01:30"
        );
        assert_eq!(
            go_date("Sat, 29 Feb 2020 23:59:59 +0000"),
            "2020-02-29T23:59:59Z"
        );
    }
}
