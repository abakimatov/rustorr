//! The Rutor database MatriX.145 downloads as `rutor.ls`: a raw DEFLATE
//! stream holding one JSON array of [`TorrentDetails`], searched through an
//! in-memory token index.

use std::{
    collections::HashMap,
    io::Read,
    path::{Path, PathBuf},
    sync::RwLock,
    time::{Duration, SystemTime},
};

use tracing::{info, warn};

use crate::TorrentDetails;

/// A local copy younger than this is not downloaded again (2 h 55 min).
const FRESH_FOR: Duration = Duration::from_secs(175 * 60);

#[derive(Default)]
struct Loaded {
    torrents: Vec<TorrentDetails>,
    index: HashMap<String, Vec<usize>>,
}

pub struct RutorDatabase {
    path: PathBuf,
    url: String,
    loaded: RwLock<Option<Loaded>>,
}

impl RutorDatabase {
    /// `path` is `<data-dir>/rutor.ls`; `url` is where updates come from.
    pub fn new(path: PathBuf, url: impl Into<String>) -> Self {
        Self {
            path,
            url: url.into(),
            loaded: RwLock::new(None),
        }
    }

    pub fn is_loaded(&self) -> bool {
        self.loaded
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .is_some()
    }

    /// Forgets the database, as the reference does when search is disabled.
    pub fn unload(&self) {
        *self
            .loaded
            .write()
            .unwrap_or_else(|error| error.into_inner()) = None;
    }

    /// Reads the local copy. A record that does not decode is skipped; an
    /// unreadable file leaves the database empty.
    pub fn load(&self) {
        let torrents = match read_database(&self.path) {
            Ok(torrents) => torrents,
            Err(error) => {
                warn!(path = %self.path.display(), %error, "cannot read the Rutor database");
                return;
            }
        };
        info!(torrents = torrents.len(), "Rutor database loaded");
        let index = build_index(&torrents);
        *self
            .loaded
            .write()
            .unwrap_or_else(|error| error.into_inner()) = Some(Loaded { torrents, index });
    }

    /// Downloads a new copy unless the local one is fresh, and loads it when
    /// its content changed. Returns whether a new copy was loaded.
    pub async fn update(&self, client: &reqwest::Client) -> bool {
        if let Ok(modified) = std::fs::metadata(&self.path).and_then(|metadata| metadata.modified())
            && SystemTime::now()
                .duration_since(modified)
                .is_ok_and(|age| age < FRESH_FOR)
        {
            return false;
        }
        let bytes = match download(client, &self.url).await {
            Ok(bytes) => bytes,
            Err(error) => {
                warn!(url = %self.url, %error, "cannot download the Rutor database");
                return false;
            }
        };
        if std::fs::read(&self.path).is_ok_and(|current| current == bytes) {
            return false;
        }
        let temporary = self.path.with_extension("tmp");
        if let Err(error) = tokio::fs::write(&temporary, &bytes).await {
            warn!(%error, "cannot store the Rutor database");
            return false;
        }
        if let Err(error) = tokio::fs::rename(&temporary, &self.path).await {
            warn!(%error, "cannot replace the Rutor database");
            return false;
        }
        self.load();
        true
    }

    /// Matching records, closest to the query first.
    pub fn search(&self, query: &str) -> Vec<TorrentDetails> {
        let loaded = self
            .loaded
            .read()
            .unwrap_or_else(|error| error.into_inner());
        let Some(loaded) = loaded.as_ref() else {
            return Vec::new();
        };
        let mut found: Vec<TorrentDetails> = search_index(&loaded.index, query)
            .into_iter()
            .map(|id| loaded.torrents[id].clone())
            .collect();
        let target: Vec<char> = clear_str(query).chars().collect();
        let distance = |torrent: &TorrentDetails| {
            let text = format!(
                "{}{}",
                clear_str(&format!("{}{}", torrent.name, torrent.names_joined())),
                torrent.year
            );
            levenshtein(&target, &text.chars().collect::<Vec<_>>())
        };
        found.sort_by_cached_key(|torrent| {
            (
                distance(torrent),
                std::cmp::Reverse(rfc3339_nanos(&torrent.create_date)),
            )
        });
        found
    }
}

async fn download(client: &reqwest::Client, url: &str) -> Result<Vec<u8>, reqwest::Error> {
    let response = client.get(url).send().await?.error_for_status()?;
    Ok(response.bytes().await?.to_vec())
}

fn read_database(path: &Path) -> std::io::Result<Vec<TorrentDetails>> {
    let mut json = Vec::new();
    flate2::read::DeflateDecoder::new(std::fs::File::open(path)?).read_to_end(&mut json)?;
    let records: Vec<serde_json::Value> = serde_json::from_slice(&json)?;
    Ok(records
        .into_iter()
        .filter_map(|record| serde_json::from_value(record).ok())
        .collect())
}

fn build_index(torrents: &[TorrentDetails]) -> HashMap<String, Vec<usize>> {
    let mut index: HashMap<String, Vec<usize>> = HashMap::new();
    for (id, torrent) in torrents.iter().enumerate() {
        for token in analyze(&torrent.title) {
            let ids = index.entry(token).or_default();
            if ids.last() != Some(&id) {
                ids.push(id);
            }
        }
    }
    index
}

/// Every query token must be indexed; the matching ids are intersected.
fn search_index(index: &HashMap<String, Vec<usize>>, query: &str) -> Vec<usize> {
    let mut result: Option<Vec<usize>> = None;
    for token in analyze(query) {
        let Some(ids) = index.get(&token) else {
            return Vec::new();
        };
        result = Some(match result {
            None => ids.clone(),
            Some(current) => intersection(&current, ids),
        });
    }
    result.unwrap_or_default()
}

fn intersection(left: &[usize], right: &[usize]) -> Vec<usize> {
    let (mut i, mut j) = (0, 0);
    let mut both = Vec::new();
    while i < left.len() && j < right.len() {
        match left[i].cmp(&right[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                both.push(left[i]);
                i += 1;
                j += 1;
            }
        }
    }
    both
}

/// The reference tokenises on anything that is not a letter or number,
/// lowercases, folds `ё` into `е` and drops stop words. It declares a
/// stemmer but never runs it.
fn analyze(text: &str) -> Vec<String> {
    text.split(|character: char| !character.is_alphabetic() && !character.is_numeric())
        .filter(|token| !token.is_empty())
        .map(|token| token.to_lowercase().replace('ё', "е"))
        .filter(|token| !is_stop_word(token))
        .collect()
}

fn is_stop_word(word: &str) -> bool {
    matches!(
        word,
        "a" | "am"
            | "an"
            | "and"
            | "are"
            | "as"
            | "at"
            | "be"
            | "by"
            | "did"
            | "do"
            | "is"
            | "of"
            | "or"
            | "s"
            | "so"
            | "t"
            | "и"
            | "в"
            | "с"
            | "со"
            | "а"
            | "но"
            | "к"
            | "у"
            | "же"
            | "бы"
            | "по"
            | "от"
            | "о"
            | "из"
            | "ну"
            | "ли"
            | "ни"
            | "нибудь"
            | "уж"
            | "ведь"
            | "ж"
            | "об"
    )
}

/// `utils.ClearStr`: lowercase ASCII digits and letters, Cyrillic `а`–`я`
/// and `ё`, nothing else.
fn clear_str(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .filter(|character| {
            character.is_ascii_digit()
                || character.is_ascii_lowercase()
                || ('а'..='я').contains(character)
                || *character == 'ё'
        })
        .collect()
}

fn levenshtein(left: &[char], right: &[char]) -> usize {
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    for (i, left_char) in left.iter().enumerate() {
        let mut current = vec![i + 1; right.len() + 1];
        for (j, right_char) in right.iter().enumerate() {
            let substitution = previous[j] + usize::from(left_char != right_char);
            current[j + 1] = substitution.min(previous[j + 1] + 1).min(current[j] + 1);
        }
        previous = current;
    }
    previous[right.len()]
}

/// Nanoseconds since the epoch for Go's RFC 3339 output; unparsable dates
/// sort as the oldest, like Go's zero time.
fn rfc3339_nanos(text: &str) -> i128 {
    parse_rfc3339(text).unwrap_or(i128::MIN)
}

fn parse_rfc3339(text: &str) -> Option<i128> {
    let number = |range: std::ops::Range<usize>| text.get(range)?.parse::<i64>().ok();
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
    let mut rest = text.get(19..)?;
    let mut nanos: i128 = 0;
    if let Some(fraction) = rest.strip_prefix('.') {
        let digits: String = fraction.chars().take_while(char::is_ascii_digit).collect();
        rest = &fraction[digits.len()..];
        let padded = format!("{digits:0<9}");
        nanos = padded.get(..9)?.parse().ok()?;
    }
    let offset_seconds = match rest {
        "Z" => 0,
        zone => {
            let sign = match zone.chars().next()? {
                '+' => 1,
                '-' => -1,
                _ => return None,
            };
            let hours: i64 = zone.get(1..3)?.parse().ok()?;
            let minutes: i64 = zone.get(4..6)?.parse().ok()?;
            sign * (hours * 3600 + minutes * 60)
        }
    };
    let days = days_from_civil(year, month, day);
    let seconds = days * 86_400 + hour * 3600 + minute * 60 + second - offset_seconds;
    Some(i128::from(seconds) * 1_000_000_000 + nanos)
}

/// Howard Hinnant's days-from-civil.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn record(title: &str, name: &str, names: &[&str], year: i64, date: &str) -> serde_json::Value {
        serde_json::json!({
            "Title": title, "Name": name, "Names": names, "Year": year, "CreateDate": date,
        })
    }

    fn database(records: &[serde_json::Value]) -> (tempfile::TempDir, RutorDatabase) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rutor.ls");
        let mut encoder =
            flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        encoder
            .write_all(serde_json::to_string(records).unwrap().as_bytes())
            .unwrap();
        std::fs::write(&path, encoder.finish().unwrap()).unwrap();
        let database = RutorDatabase::new(path, "http://127.0.0.1:9/rutor.ls");
        database.load();
        (dir, database)
    }

    fn titles(found: &[TorrentDetails]) -> Vec<&str> {
        found.iter().map(|torrent| torrent.title.as_str()).collect()
    }

    #[test]
    fn every_query_token_must_match_and_stop_words_are_ignored() {
        let (_dir, database) = database(&[
            record(
                "Фильм / Film (2021) WEB-DL",
                "Фильм",
                &["Film"],
                2021,
                "2021-05-01T12:00:00Z",
            ),
            record(
                "Сериал и фильм (2020)",
                "Сериал",
                &["Series"],
                2020,
                "2020-01-01T00:00:00Z",
            ),
            record("Ёлки (2010)", "Ёлки", &[], 2010, "2010-01-01T00:00:00Z"),
        ]);

        assert_eq!(titles(&database.search("фильм")).len(), 2);
        assert_eq!(
            titles(&database.search("и фильм 2020")),
            ["Сериал и фильм (2020)"]
        );
        assert_eq!(titles(&database.search("елки")), ["Ёлки (2010)"]);
        assert!(database.search("фильм нет").is_empty());
        assert!(database.search("   ").is_empty());
    }

    #[test]
    fn closer_names_come_first_then_newer_records() {
        let (_dir, database) = database(&[
            record("Film old", "Film", &[], 2000, "2000-01-01T00:00:00Z"),
            record("Film new", "Film", &[], 2000, "2001-01-01T00:00:00+03:00"),
            record(
                "Film remake",
                "Film Remake",
                &[],
                2000,
                "2030-01-01T00:00:00Z",
            ),
        ]);
        assert_eq!(
            titles(&database.search("film")),
            ["Film new", "Film old", "Film remake"]
        );
    }

    #[test]
    fn unloading_empties_the_database_and_bad_records_are_skipped() {
        let (_dir, database) = database(&[
            serde_json::json!({"Title": "Film", "Year": "not a number"}),
            record("Film", "Film", &[], 1, "2000-01-01T00:00:00Z"),
        ]);
        assert_eq!(database.search("film").len(), 1);
        database.unload();
        assert!(database.search("film").is_empty());
    }

    #[test]
    fn distances_and_dates_follow_the_reference_helpers() {
        let chars = |text: &str| text.chars().collect::<Vec<_>>();
        assert_eq!(levenshtein(&chars("фильм"), &chars("фильмы")), 1);
        assert_eq!(levenshtein(&chars("kitten"), &chars("sitting")), 3);
        assert_eq!(clear_str("Фильм / Film-2021 ёж!"), "фильмfilm2021ёж");
        assert_eq!(
            parse_rfc3339("2001-01-01T03:00:00+03:00"),
            parse_rfc3339("2001-01-01T00:00:00Z")
        );
        assert_eq!(parse_rfc3339("1970-01-01T00:00:01.5Z"), Some(1_500_000_000));
    }
}
