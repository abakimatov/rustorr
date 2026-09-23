use std::path::Path;

use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use rustorr_lifecycle::{TorrentFileView, TorrentView, ViewedFile};

fn escaped(value: &str) -> String {
    utf8_percent_encode(value, NON_ALPHANUMERIC)
        .to_string()
        .replace("%2E", ".")
        .replace("%2D", "-")
        .replace("%5F", "_")
}

fn file_name(path: &str) -> &str {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(path)
}

fn is_media(path: &str) -> bool {
    matches!(
        Path::new(path)
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "3gp"
            | "aac"
            | "ac3"
            | "avi"
            | "flac"
            | "flv"
            | "m2ts"
            | "m4a"
            | "m4v"
            | "mka"
            | "mkv"
            | "mov"
            | "mp3"
            | "mp4"
            | "mpeg"
            | "mpg"
            | "ogg"
            | "opus"
            | "ts"
            | "wav"
            | "webm"
    )
}

fn stem(path: &str) -> &str {
    Path::new(path)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(path)
}

fn namesakes<'a>(files: &'a [TorrentFileView], file: &TorrentFileView) -> Vec<&'a TorrentFileView> {
    let name = stem(&file.path);
    files
        .iter()
        .filter(|candidate| candidate.id != file.id && candidate.path.contains(name))
        .collect()
}

pub fn one(
    torrent: &TorrentView,
    base: &str,
    from_last: bool,
    start_index: Option<u32>,
    viewed: &[ViewedFile],
) -> String {
    let start = start_index.or_else(|| {
        from_last.then(|| {
            viewed
                .iter()
                .filter(|entry| torrent.hash.as_deref() == Some(entry.hash.to_string().as_str()))
                .map(|entry| entry.index)
                .max()
                .unwrap_or(1)
        })
    });
    let from = start
        .and_then(|index| torrent.file_stats.iter().position(|file| file.id == index))
        .unwrap_or(0);
    let hash = torrent.hash.as_deref().unwrap_or_default();
    let mut output = String::from("#EXTM3U\n");
    for (position, file) in torrent.file_stats.iter().enumerate() {
        if position < from || !is_media(&file.path) {
            continue;
        }
        let name = file_name(&file.path);
        output.push_str(&format!("#EXTINF:0,{name}\n"));
        let external = namesakes(&torrent.file_stats, file);
        if !external.is_empty() {
            output.push_str("#EXTVLCOPT:input-slave=");
            for slave in external {
                output.push_str(&format!(
                    "{base}/stream/{}?link={hash}&index={}&play#",
                    escaped(file_name(&slave.path)),
                    slave.id
                ));
            }
            output.push('\n');
        }
        output.push_str(&format!(
            "{base}/stream/{}?link={hash}&index={}&play\n",
            escaped(name),
            file.id
        ));
    }
    output
}

pub fn all(
    torrents: &[TorrentView],
    base: &str,
    merge: bool,
    category: Option<&str>,
    search: Option<&str>,
) -> String {
    let mut output = String::from("#EXTM3U\n");
    for torrent in torrents.iter().filter(|torrent| {
        let category_matches = match category {
            Some("uncategorized") => torrent.category.is_empty(),
            Some(category) => torrent.category == category,
            None => true,
        };
        let search_matches = search.is_none_or(|search| {
            torrent
                .title
                .to_ascii_lowercase()
                .contains(&search.to_ascii_lowercase())
        });
        category_matches && search_matches
    }) {
        if merge {
            output.push_str(one(torrent, base, false, None, &[]).trim_start_matches("#EXTM3U\n"));
        } else {
            output.push_str("#EXTINF:0");
            if !torrent.poster.is_empty() {
                output.push_str(&format!(" tvg-logo=\"{}\"", torrent.poster));
            }
            output.push_str(&format!(" type=\"playlist\",{}\n", torrent.title));
            output.push_str(&format!(
                "{base}/stream/{}.m3u?link={}&m3u&fn=file.m3u\n",
                escaped(&torrent.title),
                torrent.hash.as_deref().unwrap_or_default()
            ));
        }
    }
    output
}

pub fn playlist_name(requested: Option<&str>, torrent_name: &str) -> String {
    let mut name = requested
        .map(|name| name.trim_start_matches('/').to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| format!("{torrent_name}.m3u"));
    let lowercase = name.to_ascii_lowercase();
    if !lowercase.ends_with(".m3u") && !lowercase.ends_with(".m3u8") {
        name.push_str(".m3u");
    }
    name
}

pub fn etag(hash: &str, name: &str) -> String {
    let value = format!("{hash}/{name}");
    let encoded: String = value
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("\"{encoded}\"")
}
