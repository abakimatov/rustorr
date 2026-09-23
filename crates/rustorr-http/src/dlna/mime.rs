//! `server/mimetype` from MatriX.145: the types its DLNA listing shows.

/// TorrServer's own extension table, which takes precedence over Go's.
const TABLE: &[(&str, &str)] = &[
    ("image/bmp", ".bmp"),
    ("image/gif", ".gif"),
    ("image/jpeg", ".jpg,.jpeg"),
    ("image/png", ".png"),
    ("image/tiff", ".tiff,.tif"),
    ("audio/x-aac", ".aac"),
    ("audio/dsd", ".dsd,.dsf,.dff"),
    ("audio/flac", ".flac"),
    ("audio/mpeg", ".mpga,.mpega,.mp2,.mp3,.m4a"),
    ("audio/ogg", ".oga,.ogg,.spx"),
    // Registered after audio/ogg, so it wins for .opus.
    ("audio/opus", ".opus"),
    ("audio/weba", ".weba"),
    ("audio/x-ape", ".ape"),
    ("audio/x-wav", ".wav"),
    ("video/dv", ".dif,.dv"),
    ("video/fli", ".fli"),
    ("video/mp4", ".mp4"),
    ("video/mpeg", ".mpeg,.mpg,.mpe"),
    ("video/x-matroska", ".mpv,.mkv"),
    ("video/mp2t", ".ts,.m2ts,.mts"),
    ("video/ogg", ".ogv"),
    ("video/webm", ".webm"),
    ("video/x-ms-vob", ".vob"),
    ("video/x-msvideo", ".avi"),
    ("video/x-quicktime", ".qt,.mov"),
    ("text/srt", ".srt"),
    ("text/smi", ".smi"),
    ("text/ssa", ".ssa"),
    ("application/vnd.rn-realmedia-vbr", ".rmvb"),
];

/// `MimeTypeByPath` for a torrent file path: by extension (a trailing
/// `.part` ignored), `None` when unknown — the reference then fails to sniff
/// the file, which only exists inside the torrent, and skips it.
pub(crate) fn by_path(path: &str) -> Option<String> {
    let base = path.rsplit('/').next().unwrap_or(path);
    let base = base.strip_suffix(".part").unwrap_or(base);
    let extension = base.rfind('.').map(|dot| &base[dot..])?;
    let lower = extension.to_ascii_lowercase();
    let found = TABLE
        .iter()
        .find(|(_, extensions)| {
            extensions
                .split(',')
                .any(|known| known == extension || known == lower)
        })
        .map(|(kind, _)| (*kind).to_owned())
        .or_else(|| {
            mime_guess::from_ext(&lower[1..])
                .first()
                .map(|mime| mime.essence_str().to_owned())
        })?;
    Some(match found.as_str() {
        "video/mp2t" => "video/mpeg".into(),
        "video/x-msvideo" => "video/avi".into(),
        _ => found,
    })
}

/// `IsMedia`: video (including RealMedia), audio or image.
pub(crate) fn is_media(kind: &str) -> bool {
    kind.starts_with("video/")
        || kind.starts_with("audio/")
        || kind.starts_with("image/")
        || kind == "application/vnd.rn-realmedia-vbr"
}

/// `Type`: the part before the slash, which names the UPnP item class.
pub(crate) fn major(kind: &str) -> &str {
    kind.split('/').next().unwrap_or(kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reference_table_wins_and_is_remapped() {
        assert_eq!(by_path("Show/ep.MKV").as_deref(), Some("video/x-matroska"));
        assert_eq!(by_path("a.ts").as_deref(), Some("video/mpeg"));
        assert_eq!(by_path("a.avi").as_deref(), Some("video/avi"));
        assert_eq!(by_path("a.opus").as_deref(), Some("audio/opus"));
        assert_eq!(by_path("a.mp4.part").as_deref(), Some("video/mp4"));
        assert_eq!(by_path("README").as_deref(), None);
        assert!(is_media("application/vnd.rn-realmedia-vbr"));
        assert!(!is_media("text/srt"));
        assert_eq!(major("video/mp4"), "video");
    }
}
