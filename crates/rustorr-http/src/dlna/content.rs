//! TorrServer's ContentDirectory (`server/dlna/list.go`) over the client
//! core: `Torrents`, categories, torrents and their media files.

use std::{
    collections::HashMap,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use rustorr_lifecycle::{AddTorrent, InfoHash, TorrentCommand, TorrentReply, TorrentView};

use super::{
    DlnaDevice, clean,
    didl::{Entry, Object, Resource, didl_lite},
    mime,
    soap::UpnpError,
};

const FOLDER: &str = "object.container.storageFolder";
const CATEGORIES: [&str; 5] = ["movie", "tv", "music", "other", "uncategorized"];
/// Go's `url.PathEscape` keeps these besides letters and digits.
const PATH_SEGMENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~')
    .remove(b'$')
    .remove(b'&')
    .remove(b'+')
    .remove(b':')
    .remove(b'=')
    .remove(b'@');

fn path_escape(text: &str) -> String {
    utf8_percent_encode(text, PATH_SEGMENT).to_string()
}

/// `Browse`: the DIDL-Lite result, the number returned and the total.
pub(crate) async fn browse(
    device: &DlnaDevice,
    object_id: &str,
    flag: &str,
    start: i64,
    count: i64,
    host: &str,
) -> Result<(String, usize, usize), UpnpError> {
    let path = object_path(object_id)?;
    match flag {
        "BrowseDirectChildren" => {
            let entries = children(device, &path, host).await;
            let total = entries.len();
            let start = usize::try_from(start).unwrap_or(0).min(total);
            let mut page = &entries[start..];
            if let Ok(count) = usize::try_from(count)
                && count != 0
                && count < page.len()
            {
                page = &page[..count];
            }
            let xml: String = page.iter().map(Entry::xml).collect();
            Ok((didl_lite(&xml), page.len(), total))
        }
        "BrowseMetadata" => {
            let entry = metadata(device, &path)
                .await
                .ok_or_else(|| UpnpError::new(501, "meta not found"))?;
            Ok((didl_lite(&entry.xml()), 1, 1))
        }
        flag => Err(UpnpError::new(
            600,
            format!("unhandled browse flag: {flag}"),
        )),
    }
}

/// `objectFromID`: query-unescaped, `0` is the root, cleaned, absolute.
fn object_path(id: &str) -> Result<String, UpnpError> {
    let unescaped = query_unescape(id).map_err(|message| UpnpError::new(701, message))?;
    let path = if unescaped == "0" {
        "/".to_owned()
    } else {
        unescaped
    };
    let path = clean(&path);
    if !path.starts_with('/') {
        return Err(UpnpError::new(701, format!("bad ObjectID {path}")));
    }
    Ok(path)
}

/// `url.QueryUnescape` with its error text.
fn query_unescape(text: &str) -> Result<String, String> {
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let escape = text
                .get(index..(index + 3).min(text.len()))
                .unwrap_or_default();
            if escape.len() < 3 || !escape[1..].bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(format!("invalid URL escape \"{escape}\""));
            }
            index += 3;
        } else {
            index += 1;
        }
    }
    Ok(percent_decode_str(&text.replace('+', " "))
        .decode_utf8_lossy()
        .into_owned())
}

fn today() -> String {
    date(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_secs()) as i64,
    )
}

/// `upnpav.Timestamp`: the calendar date of a Unix time, in UTC.
fn date(timestamp: i64) -> String {
    let days = timestamp.div_euclid(86_400);
    // Howard Hinnant's civil-from-days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
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
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

fn folder(id: &str, parent: &str, title: &str, date: String) -> Object {
    Object {
        id: id.into(),
        parent_id: parent.into(),
        restricted: 1,
        title: title.into(),
        class: FOLDER.into(),
        date,
        ..Object::default()
    }
}

async fn torrents(device: &DlnaDevice) -> Vec<TorrentView> {
    match device.core.torrents(TorrentCommand::List).await {
        Ok(TorrentReply::List(torrents)) => torrents,
        _ => Vec::new(),
    }
}

fn hash_of(torrent: &TorrentView) -> String {
    torrent
        .hash
        .clone()
        .unwrap_or_default()
        .to_ascii_lowercase()
}

fn normalize_category(category: &str) -> String {
    let category = category.trim().to_lowercase();
    if category.is_empty() {
        "uncategorized".into()
    } else {
        category
    }
}

fn category_title(category: &str) -> &str {
    match category {
        "movie" => "Movies",
        "tv" => "TV Shows",
        "music" => "Music",
        "other" => "Other",
        "uncategorized" => "Uncategorized",
        other => other,
    }
}

/// `isHashPath`: the last element is 40 hex digits.
fn is_hash_path(path: &str) -> bool {
    let base = base(path);
    base.len() == 40 && base.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Go's `path.Base`.
fn base(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return if path.is_empty() { "." } else { "/" };
    }
    trimmed.rsplit('/').next().unwrap_or(trimmed)
}

/// Go's `path.Dir`.
fn dir(path: &str) -> String {
    match path.rfind('/') {
        Some(index) => clean(&path[..=index]),
        None => ".".into(),
    }
}

fn torrent_container(torrent: &TorrentView, parent: &str) -> Entry {
    let mut object = folder(
        &format!("%2F{}", hash_of(torrent)),
        parent,
        &torrent.title.replace('/', "|"),
        date(torrent.timestamp),
    );
    object.icon.clone_from(&torrent.poster);
    object.album_art_uri.clone_from(&torrent.poster);
    Entry::container(object, 1)
}

/// `onBrowse`.
async fn children(device: &DlnaDevice, path: &str, host: &str) -> Vec<Entry> {
    if path == "/" {
        let count = torrents(device).await.len();
        return vec![Entry::container(
            folder("%2FTR", "0", "Torrents", today()),
            count,
        )];
    }
    if path == "/TR" {
        return categories(&torrents(device).await);
    }
    if let Some(category) = path.strip_prefix("/TR/") {
        return by_category(&torrents(device).await, category);
    }
    if is_hash_path(path) {
        let list = torrents(device).await;
        let Some(torrent) = list.iter().find(|torrent| path.contains(&hash_of(torrent))) else {
            return Vec::new();
        };
        // A torrent without loaded metadata offers a button to load it.
        if torrent.stat == 5 || torrent.file_stats.is_empty() {
            let parent = format!("%2F{}", hash_of(torrent));
            return vec![Entry::container(
                folder(&format!("{parent}%2FLD"), &parent, "Load Torrent", today()),
                1,
            )];
        }
        return load(device, path, host).await;
    }
    if base(path) == "LD" {
        return load(device, path, host).await;
    }
    Vec::new()
}

fn categories(list: &[TorrentView]) -> Vec<Entry> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for torrent in list {
        *counts
            .entry(normalize_category(&torrent.category))
            .or_default() += 1;
    }
    if counts.is_empty() {
        return vec![Entry::container(
            folder("%2FNT", "%2FTR", "No Torrents", today()),
            0,
        )];
    }
    let mut order: Vec<String> = CATEGORIES
        .iter()
        .filter(|known| counts.contains_key(**known))
        .map(|known| (*known).to_owned())
        .collect();
    let mut rest: Vec<String> = counts
        .keys()
        .filter(|category| !CATEGORIES.contains(&category.as_str()))
        .cloned()
        .collect();
    rest.sort();
    order.extend(rest);
    order
        .into_iter()
        .map(|category| {
            let id = path_escape(&format!("/TR/{category}"));
            Entry::container(
                folder(&id, "%2FTR", category_title(&category), today()),
                counts[&category],
            )
        })
        .collect()
}

fn by_category(list: &[TorrentView], escaped: &str) -> Vec<Entry> {
    let category = normalize_category(&percent_decode_str(escaped).decode_utf8_lossy());
    let parent = path_escape(&format!("/TR/{category}"));
    let mut filtered: Vec<&TorrentView> = list
        .iter()
        .filter(|torrent| normalize_category(&torrent.category) == category)
        .collect();
    filtered.sort_by(|left, right| left.title.cmp(&right.title));
    if filtered.is_empty() {
        return vec![Entry::container(
            folder("%2FNT", &parent, "Empty", today()),
            0,
        )];
    }
    filtered
        .into_iter()
        .map(|torrent| torrent_container(torrent, &parent))
        .collect()
}

/// `loadTorrent`: loads the torrent if needed, waits up to a minute for its
/// file list and lists its media files.
async fn load(device: &DlnaDevice, path: &str, host: &str) -> Vec<Entry> {
    let mut hash = base(&dir(path)).to_owned();
    if hash == "/" {
        hash = base(path).to_owned();
    }
    let Ok(info_hash) = hash.to_ascii_lowercase().parse::<InfoHash>() else {
        return Vec::new();
    };
    if hash.len() != 40 {
        return Vec::new();
    }
    let known = matches!(
        device.core.torrents(TorrentCommand::Get(info_hash)).await,
        Ok(TorrentReply::Torrent(Some(_)))
    );
    if !known {
        return Vec::new();
    }
    let deadline = Instant::now() + Duration::from_secs(60);
    let torrent = loop {
        let reply = device
            .core
            .torrents(TorrentCommand::Add(AddTorrent {
                link: info_hash.to_string(),
                ..AddTorrent::default()
            }))
            .await;
        if let Ok(TorrentReply::Torrent(Some(torrent))) = reply
            && !torrent.file_stats.is_empty()
        {
            break *torrent;
        }
        if Instant::now() > deadline {
            return Vec::new();
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let parent = format!("%2F{}", hash_of(&torrent));
    let name = torrent.name.clone().unwrap_or_default();
    let prefix = format!("{}/", base(&name));
    torrent
        .file_stats
        .iter()
        .filter_map(|file| {
            let kind = mime::by_path(&file.path).filter(|kind| mime::is_media(kind))?;
            let title = file.path.strip_prefix(&prefix).unwrap_or(&file.path);
            let object = Object {
                id: format!("{parent}%2F{}", path_escape(&file.path)),
                parent_id: parent.clone(),
                restricted: 1,
                title: title.to_owned(),
                class: format!("object.item.{}Item", mime::major(&kind)),
                date: today(),
                ..Object::default()
            };
            Some(Entry::Item {
                object,
                resources: vec![Resource {
                    protocol_info: format!(
                        "http-get:*:{kind}:DLNA.ORG_OP=11;DLNA.ORG_CI=0;DLNA.ORG_FLAGS=01700000000000000000000000000000"
                    ),
                    url: link(host, device.web_port, &format!("play/{}/{}", hash_of(&torrent), file.id)),
                    size: file.length,
                }],
            })
        })
        .collect()
}

/// `getLink`: the request's host with the web port.
fn link(host: &str, port: u16, path: &str) -> String {
    let mut host = if host.starts_with("http") {
        host.to_owned()
    } else {
        format!("http://{host}")
    };
    if let Some(colon) = host.rfind(':')
        && colon > 7
    {
        host.truncate(colon);
    }
    format!("{host}:{port}/{path}")
}

/// `onBrowseMeta` (`getTorrentMeta`).
async fn metadata(device: &DlnaDevice, path: &str) -> Option<Entry> {
    let searchable = |mut object: Object| {
        object.searchable = 1;
        object
    };
    if path == "/" {
        return Some(Entry::container(
            searchable(folder("0", "-1", "TorrServer", today())),
            1,
        ));
    }
    if let Some(category) = path.strip_prefix("/TR/") {
        let category = normalize_category(&percent_decode_str(category).decode_utf8_lossy());
        let count = torrents(device)
            .await
            .iter()
            .filter(|torrent| normalize_category(&torrent.category) == category)
            .count();
        return Some(Entry::container(
            searchable(folder(
                &path_escape(path),
                "%2FTR",
                category_title(&category),
                today(),
            )),
            count,
        ));
    }
    if base(path) == "TR" {
        let count = torrents(device).await.len();
        return Some(Entry::container(
            searchable(folder("%2FTR", "0", "Torrents", today())),
            count,
        ));
    }
    if is_hash_path(path) {
        let list = torrents(device).await;
        let torrent = list
            .iter()
            .find(|torrent| path.contains(&hash_of(torrent)))?;
        let object = Object {
            id: format!("%2F{}", hash_of(torrent)),
            parent_id: "%2FTR".into(),
            restricted: 1,
            title: torrent.title.clone(),
            date: date(torrent.timestamp),
            ..Object::default()
        };
        return Some(Entry::container(object, 1));
    }
    let parent = path_escape(&dir(path));
    if base(path) == "LD" {
        let object = Object {
            id: format!("{parent}%2FLD"),
            parent_id: parent,
            restricted: 1,
            searchable: 1,
            title: "Load Torrents".into(),
            date: today(),
            ..Object::default()
        };
        return Some(Entry::container(object, 1));
    }
    let object = Object {
        id: path_escape(path),
        parent_id: parent,
        restricted: 1,
        searchable: 1,
        title: base(path).into(),
        date: today(),
        ..Object::default()
    };
    Some(Entry::container(object, 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_ids_decode_and_clean() {
        assert_eq!(object_path("0").unwrap(), "/");
        assert_eq!(object_path("%2FTR%2Fmovie").unwrap(), "/TR/movie");
        // Observed from MatriX.145.
        assert_eq!(
            object_path("relative").unwrap_err(),
            UpnpError::new(701, "bad ObjectID relative")
        );
        assert_eq!(
            object_path("%zz").unwrap_err(),
            UpnpError::new(701, "invalid URL escape \"%zz\"")
        );
    }

    #[test]
    fn escapes_links_and_dates_follow_go() {
        assert_eq!(path_escape("/TR/movie"), "%2FTR%2Fmovie");
        assert_eq!(path_escape("a b:c&d"), "a%20b:c&d");
        assert_eq!(
            link("dlna.invalid:9080", 8090, "play/x/1"),
            "http://dlna.invalid:8090/play/x/1"
        );
        assert_eq!(link("[::1]:9080", 8090, "p"), "http://[::1]:8090/p");
        assert_eq!(date(0), "1970-01-01");
        assert_eq!(date(1_790_150_400), "2026-09-23");
    }

    #[test]
    fn paths_split_like_go() {
        assert_eq!(base("/abc/LD"), "LD");
        assert_eq!(dir("/abc/LD"), "/abc");
        assert_eq!(dir("/abc"), "/");
        assert!(is_hash_path("/0123456789abcdef0123456789abcdef01234567"));
        assert!(!is_hash_path("/TR"));
    }
}
