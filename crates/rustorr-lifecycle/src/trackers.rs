//! MatriX.145 tracker policy: `RetrackersMode` decides how the default tracker
//! list joins a torrent's own trackers, and `<data-dir>/trackers.txt` adds one
//! more tier in every mode.

use crate::Settings;

/// Trackers a torrent announces to and shows in its magnet link, distinct and
/// in tier order: the torrent's own, then the default list, then the file.
pub fn announce_list(own: &[String], settings: &Settings, file: &[String]) -> Vec<String> {
    let mut tiers: Vec<String> = match settings.retrackers_mode {
        1 => own
            .iter()
            .cloned()
            .chain(default_trackers(settings))
            .collect(),
        2 => Vec::new(),
        3 => default_trackers(settings),
        _ => own.to_vec(),
    };
    tiers.extend(file.iter().cloned());
    let mut seen = std::collections::HashSet::new();
    tiers.retain(|tracker| seen.insert(tracker.clone()));
    tiers
}

/// `DefaultTrackers` from settings, or the built-in list when it is blank.
pub fn default_trackers(settings: &Settings) -> Vec<String> {
    let parsed = parse_default_lines(&settings.default_trackers);
    if parsed.is_empty() {
        parse_default_lines(&Settings::default().default_trackers)
    } else {
        parsed
    }
}

fn parse_default_lines(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .filter(|line| {
            ["udp", "http", "wss"]
                .iter()
                .any(|scheme| line.starts_with(scheme))
        })
        .map(str::to_owned)
        .collect()
}

/// `trackers.txt` accepts only `udp` and `http(s)` lines, unlike the default
/// list, which also takes `wss`.
pub fn parse_trackers_file(text: &str) -> Vec<String> {
    text.split('\n')
        .map(str::trim)
        .filter(|line| line.starts_with("udp") || line.starts_with("http"))
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(mode: i32, defaults: &str) -> Settings {
        Settings {
            retrackers_mode: mode,
            default_trackers: defaults.into(),
            ..Settings::default()
        }
    }

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn each_mode_combines_own_default_and_file_trackers_like_the_reference() {
        let own = strings(&["http://own/announce", "udp://shared:1"]);
        let file = strings(&["http://file/announce"]);
        let defaults = "udp://shared:1\n# comment\n\nwss://default\nftp://ignored";

        assert_eq!(
            announce_list(&own, &settings(0, defaults), &file),
            [
                "http://own/announce",
                "udp://shared:1",
                "http://file/announce"
            ]
        );
        assert_eq!(
            announce_list(&own, &settings(1, defaults), &file),
            [
                "http://own/announce",
                "udp://shared:1",
                "wss://default",
                "http://file/announce"
            ]
        );
        assert_eq!(
            announce_list(&own, &settings(2, defaults), &file),
            ["http://file/announce"]
        );
        assert_eq!(
            announce_list(&own, &settings(3, defaults), &[]),
            ["udp://shared:1", "wss://default"]
        );
    }

    #[test]
    fn blank_default_trackers_fall_back_to_the_built_in_list() {
        let fallback = default_trackers(&settings(1, " \n"));
        assert_eq!(
            fallback.first().map(String::as_str),
            Some("http://retracker.local/announce")
        );
        assert_eq!(fallback.len(), 14);
    }

    #[test]
    fn the_trackers_file_takes_udp_and_http_but_not_wss() {
        assert_eq!(
            parse_trackers_file(" http://a/announce \r\nwss://b\nudp://c:1\n#x"),
            ["http://a/announce", "udp://c:1"]
        );
    }
}
