use std::time::SystemTime;

use rusqlite::{OptionalExtension, params};
use rustorr_domain::InfoHash;

use crate::{
    Error, State,
    error::database,
    values::{info_hash, system_time, unix_seconds},
};

/// A saved torrent as listed. The `.torrent` bytes are kept apart, see
/// [`State::metainfo`]. `added_at` is stored with one-second precision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogEntry {
    pub hash: InfoHash,
    pub title: String,
    pub poster: String,
    pub category: String,
    /// Opaque client-supplied text, returned as it was saved.
    pub data: String,
    pub added_at: SystemTime,
    pub size: u64,
}

const COLUMNS: &str = "info_hash, title, poster, category, data, added_at, size";

struct Stored {
    hash: Vec<u8>,
    title: String,
    poster: String,
    category: String,
    data: String,
    added_at: i64,
    size: i64,
}

impl Stored {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            hash: row.get(0)?,
            title: row.get(1)?,
            poster: row.get(2)?,
            category: row.get(3)?,
            data: row.get(4)?,
            added_at: row.get(5)?,
            size: row.get(6)?,
        })
    }

    fn into_entry(self) -> Result<CatalogEntry, Error> {
        Ok(CatalogEntry {
            hash: info_hash(&self.hash)?,
            title: self.title,
            poster: self.poster,
            category: self.category,
            data: self.data,
            added_at: system_time(self.added_at)?,
            size: u64::try_from(self.size).map_err(|_| Error::InvalidValue {
                field: "torrent size",
                reason: "stored value is negative",
            })?,
        })
    }
}

impl State {
    /// Saves a torrent, replacing any earlier entry with the same hash.
    pub fn save_torrent(&self, entry: &CatalogEntry, metainfo: &[u8]) -> Result<(), Error> {
        let added_at = unix_seconds(entry.added_at)?;
        let size = i64::try_from(entry.size).map_err(|_| Error::InvalidValue {
            field: "torrent size",
            reason: "does not fit in 63 bits",
        })?;
        self.connection()
            .execute(
                "INSERT INTO torrents (info_hash, title, poster, category, data, added_at, size, metainfo)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT (info_hash) DO UPDATE SET
                    title = excluded.title, poster = excluded.poster,
                    category = excluded.category, data = excluded.data,
                    added_at = excluded.added_at, size = excluded.size,
                    metainfo = excluded.metainfo",
                params![
                    entry.hash.as_bytes().as_slice(),
                    entry.title,
                    entry.poster,
                    entry.category,
                    entry.data,
                    added_at,
                    size,
                    metainfo,
                ],
            )
            .map_err(database("save torrent"))?;
        Ok(())
    }

    pub fn torrent(&self, hash: InfoHash) -> Result<Option<CatalogEntry>, Error> {
        let stored = self
            .connection()
            .query_row(
                &format!("SELECT {COLUMNS} FROM torrents WHERE info_hash = ?1"),
                [hash.as_bytes().as_slice()],
                Stored::from_row,
            )
            .optional()
            .map_err(database("load torrent"))?;
        stored.map(Stored::into_entry).transpose()
    }

    /// The saved `.torrent` bytes, needed only when the torrent is loaded.
    pub fn metainfo(&self, hash: InfoHash) -> Result<Option<Vec<u8>>, Error> {
        self.connection()
            .query_row(
                "SELECT metainfo FROM torrents WHERE info_hash = ?1",
                [hash.as_bytes().as_slice()],
                |row| row.get(0),
            )
            .optional()
            .map_err(database("load torrent metadata"))
    }

    /// Oldest first, ties broken by hash so the order is stable.
    pub fn list_torrents(&self) -> Result<Vec<CatalogEntry>, Error> {
        let connection = self.connection();
        let mut statement = connection
            .prepare(&format!(
                "SELECT {COLUMNS} FROM torrents ORDER BY added_at, info_hash"
            ))
            .map_err(database("list torrents"))?;
        let stored = statement
            .query_map([], Stored::from_row)
            .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
            .map_err(database("list torrents"))?;
        stored.into_iter().map(Stored::into_entry).collect()
    }

    /// Returns whether a torrent was removed.
    pub fn remove_torrent(&self, hash: InfoHash) -> Result<bool, Error> {
        let removed = self
            .connection()
            .execute(
                "DELETE FROM torrents WHERE info_hash = ?1",
                [hash.as_bytes().as_slice()],
            )
            .map_err(database("remove torrent"))?;
        Ok(removed > 0)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn hash(n: u8) -> InfoHash {
        InfoHash::from_bytes([n; 20])
    }

    fn at(seconds: u64) -> SystemTime {
        std::time::UNIX_EPOCH + Duration::from_secs(seconds)
    }

    fn entry(n: u8, added_at: u64) -> CatalogEntry {
        CatalogEntry {
            hash: hash(n),
            title: format!("title {n}"),
            poster: String::new(),
            category: "movie".into(),
            data: String::new(),
            added_at: at(added_at),
            size: 8_388_608,
        }
    }

    #[test]
    fn a_saved_torrent_comes_back_field_for_field() {
        let state = State::open_in_memory().unwrap();
        let saved = CatalogEntry {
            title: "Фильм — 日本語 \"quoted\"".into(),
            poster: "http://x/p.jpg?a=1&b=2".into(),
            data: "{\"a\":\n [1, 2]}".into(),
            ..entry(1, 1_789_915_523)
        };

        state.save_torrent(&saved, b"d4:infod").unwrap();

        assert_eq!(state.torrent(hash(1)).unwrap(), Some(saved));
    }

    #[test]
    fn metainfo_is_stored_verbatim_including_zero_bytes() {
        let state = State::open_in_memory().unwrap();
        let bytes: Vec<u8> = (0..=255).chain([0, 0, 255]).collect();

        state.save_torrent(&entry(1, 10), &bytes).unwrap();

        assert_eq!(state.metainfo(hash(1)).unwrap(), Some(bytes));
    }

    #[test]
    fn unknown_torrents_are_none() {
        let state = State::open_in_memory().unwrap();

        assert_eq!(state.torrent(hash(9)).unwrap(), None);
        assert_eq!(state.metainfo(hash(9)).unwrap(), None);
    }

    #[test]
    fn saving_again_replaces_the_entry_instead_of_adding_one() {
        let state = State::open_in_memory().unwrap();
        state.save_torrent(&entry(1, 10), b"old").unwrap();

        let renamed = CatalogEntry {
            title: "renamed".into(),
            added_at: at(99),
            ..entry(1, 10)
        };
        state.save_torrent(&renamed, b"new").unwrap();

        assert_eq!(state.list_torrents().unwrap(), [renamed]);
        assert_eq!(state.metainfo(hash(1)).unwrap(), Some(b"new".to_vec()));
    }

    #[test]
    fn listing_is_oldest_first_with_the_hash_breaking_ties() {
        let state = State::open_in_memory().unwrap();
        for (n, added_at) in [(3, 30), (2, 20), (5, 20), (1, 40)] {
            state.save_torrent(&entry(n, added_at), b"m").unwrap();
        }

        let order: Vec<u8> = state
            .list_torrents()
            .unwrap()
            .iter()
            .map(|e| e.hash.as_bytes()[0])
            .collect();

        assert_eq!(order, [2, 5, 3, 1]);
    }

    #[test]
    fn removing_reports_whether_anything_was_removed() {
        let state = State::open_in_memory().unwrap();
        state.save_torrent(&entry(1, 10), b"m").unwrap();

        assert!(state.remove_torrent(hash(1)).unwrap());
        assert!(!state.remove_torrent(hash(1)).unwrap());
        assert_eq!(state.torrent(hash(1)).unwrap(), None);
        assert_eq!(state.metainfo(hash(1)).unwrap(), None);
    }

    #[test]
    fn sub_second_precision_is_dropped() {
        let state = State::open_in_memory().unwrap();
        let precise = CatalogEntry {
            added_at: at(50) + Duration::from_millis(900),
            ..entry(1, 0)
        };

        state.save_torrent(&precise, b"m").unwrap();

        assert_eq!(state.torrent(hash(1)).unwrap().unwrap().added_at, at(50));
    }

    #[test]
    fn values_that_cannot_be_stored_are_rejected_and_nothing_is_written() {
        let state = State::open_in_memory().unwrap();
        let too_big = CatalogEntry {
            size: u64::MAX,
            ..entry(1, 10)
        };
        let before_1970 = CatalogEntry {
            added_at: std::time::UNIX_EPOCH - Duration::from_secs(1),
            ..entry(2, 10)
        };

        for rejected in [too_big, before_1970] {
            assert!(matches!(
                state.save_torrent(&rejected, b"m"),
                Err(Error::InvalidValue { .. })
            ));
        }
        assert!(state.list_torrents().unwrap().is_empty());
    }

    #[test]
    fn the_catalog_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rustorr.db");
        {
            let state = State::open(&path).unwrap();
            state.save_torrent(&entry(1, 10), b"meta").unwrap();
            state.save_torrent(&entry(2, 20), b"meta2").unwrap();
        }

        let state = State::open(&path).unwrap();

        assert_eq!(state.list_torrents().unwrap(), [entry(1, 10), entry(2, 20)]);
        assert_eq!(state.metainfo(hash(2)).unwrap(), Some(b"meta2".to_vec()));
    }
}
