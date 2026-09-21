use rusqlite::params;
use rustorr_domain::{FileIndex, InfoHash};

use crate::{Error, State, error::database, values::info_hash};

/// A file that was viewed, with the playback position reached in it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewedEntry {
    pub torrent: InfoHash,
    pub file: FileIndex,
    pub timecode: f64,
}

impl State {
    /// Marks a file as viewed, or updates its timecode if it already is.
    ///
    /// Viewed history is independent of the catalog: it may name torrents that
    /// are not saved, and removing a torrent from the catalog leaves it alone.
    pub fn set_viewed(&self, entry: &ViewedEntry) -> Result<(), Error> {
        if !entry.timecode.is_finite() {
            return Err(Error::InvalidValue {
                field: "timecode",
                reason: "is not a finite number",
            });
        }
        self.connection()
            .execute(
                "INSERT INTO viewed (info_hash, file_index, timecode) VALUES (?1, ?2, ?3)
                 ON CONFLICT (info_hash, file_index) DO UPDATE SET timecode = excluded.timecode",
                params![
                    entry.torrent.as_bytes().as_slice(),
                    i64::from(entry.file.zero_based()),
                    entry.timecode,
                ],
            )
            .map(drop)
            .map_err(database("save viewed file"))
    }

    /// Ordered by torrent hash, then file index.
    pub fn list_viewed(&self) -> Result<Vec<ViewedEntry>, Error> {
        let connection = self.connection();
        let mut statement = connection
            .prepare(
                "SELECT info_hash, file_index, timecode FROM viewed ORDER BY info_hash, file_index",
            )
            .map_err(database("list viewed files"))?;
        let stored = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, f64>(2)?,
                ))
            })
            .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
            .map_err(database("list viewed files"))?;

        stored
            .into_iter()
            .map(|(hash, file, timecode)| {
                let file = u32::try_from(file).map_err(|_| Error::InvalidValue {
                    field: "file index",
                    reason: "stored value is out of range",
                })?;
                Ok(ViewedEntry {
                    torrent: info_hash(&hash)?,
                    file: FileIndex::from_zero_based(file),
                    timecode,
                })
            })
            .collect()
    }

    /// Forgets one viewed file, or every viewed file of the torrent when `file`
    /// is `None`. Returns how many entries were removed.
    pub fn remove_viewed(
        &self,
        torrent: InfoHash,
        file: Option<FileIndex>,
    ) -> Result<usize, Error> {
        let connection = self.connection();
        let hash = torrent.as_bytes().as_slice();
        match file {
            Some(file) => connection.execute(
                "DELETE FROM viewed WHERE info_hash = ?1 AND file_index = ?2",
                params![hash, i64::from(file.zero_based())],
            ),
            None => connection.execute("DELETE FROM viewed WHERE info_hash = ?1", [hash]),
        }
        .map_err(database("remove viewed files"))
    }
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use crate::CatalogEntry;

    use super::*;

    fn hash(n: u8) -> InfoHash {
        InfoHash::from_bytes([n; 20])
    }

    fn viewed(n: u8, file: u32, timecode: f64) -> ViewedEntry {
        ViewedEntry {
            torrent: hash(n),
            file: FileIndex::from_zero_based(file),
            timecode,
        }
    }

    #[test]
    fn a_viewed_file_comes_back_with_its_timecode() {
        let state = State::open_in_memory().unwrap();

        state.set_viewed(&viewed(1, 0, 1234.5)).unwrap();

        assert_eq!(state.list_viewed().unwrap(), [viewed(1, 0, 1234.5)]);
    }

    #[test]
    fn marking_again_updates_the_timecode_instead_of_adding_a_row() {
        let state = State::open_in_memory().unwrap();
        state.set_viewed(&viewed(1, 2, 10.0)).unwrap();

        state.set_viewed(&viewed(1, 2, 99.25)).unwrap();

        assert_eq!(state.list_viewed().unwrap(), [viewed(1, 2, 99.25)]);
    }

    #[test]
    fn listing_is_ordered_by_hash_then_file() {
        let state = State::open_in_memory().unwrap();
        for (n, file) in [(2, 1), (1, 3), (2, 0), (1, 0)] {
            state.set_viewed(&viewed(n, file, 0.0)).unwrap();
        }

        let order: Vec<(u8, u32)> = state
            .list_viewed()
            .unwrap()
            .iter()
            .map(|e| (e.torrent.as_bytes()[0], e.file.zero_based()))
            .collect();

        assert_eq!(order, [(1, 0), (1, 3), (2, 0), (2, 1)]);
    }

    #[test]
    fn one_file_or_a_whole_torrent_can_be_removed() {
        let state = State::open_in_memory().unwrap();
        for (n, file) in [(1, 0), (1, 1), (2, 0)] {
            state.set_viewed(&viewed(n, file, 0.0)).unwrap();
        }

        assert_eq!(
            state
                .remove_viewed(hash(1), Some(FileIndex::from_zero_based(1)))
                .unwrap(),
            1
        );
        assert_eq!(state.list_viewed().unwrap().len(), 2);

        assert_eq!(state.remove_viewed(hash(1), None).unwrap(), 1);
        assert_eq!(state.list_viewed().unwrap(), [viewed(2, 0, 0.0)]);

        assert_eq!(state.remove_viewed(hash(9), None).unwrap(), 0);
    }

    #[test]
    fn a_timecode_that_is_not_a_finite_number_is_rejected() {
        let state = State::open_in_memory().unwrap();

        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(matches!(
                state.set_viewed(&viewed(1, 0, bad)),
                Err(Error::InvalidValue { .. })
            ));
        }
        assert!(state.list_viewed().unwrap().is_empty());
    }

    #[test]
    fn the_largest_file_index_round_trips() {
        let state = State::open_in_memory().unwrap();

        state.set_viewed(&viewed(1, u32::MAX, 0.0)).unwrap();

        assert_eq!(state.list_viewed().unwrap(), [viewed(1, u32::MAX, 0.0)]);
    }

    #[test]
    fn history_is_independent_of_the_catalog() {
        let state = State::open_in_memory().unwrap();
        state.set_viewed(&viewed(1, 0, 5.0)).unwrap();
        let saved = CatalogEntry {
            hash: hash(1),
            title: String::new(),
            poster: String::new(),
            category: String::new(),
            data: String::new(),
            added_at: SystemTime::UNIX_EPOCH,
            size: 1,
        };

        state.save_torrent(&saved, b"m").unwrap();
        state.remove_torrent(hash(1)).unwrap();

        assert_eq!(state.list_viewed().unwrap(), [viewed(1, 0, 5.0)]);
    }

    #[test]
    fn viewed_files_survive_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rustorr.db");
        State::open(&path)
            .unwrap()
            .set_viewed(&viewed(1, 4, 7.5))
            .unwrap();

        let state = State::open(&path).unwrap();

        assert_eq!(state.list_viewed().unwrap(), [viewed(1, 4, 7.5)]);
    }

    #[test]
    fn concurrent_writers_do_not_lose_updates() {
        let state = std::sync::Arc::new(State::open_in_memory().unwrap());

        std::thread::scope(|scope| {
            for worker in 0..4u32 {
                let state = state.clone();
                scope.spawn(move || {
                    for file in 0..25 {
                        state
                            .set_viewed(&viewed(1, worker * 100 + file, f64::from(file)))
                            .unwrap();
                    }
                });
            }
        });

        assert_eq!(state.list_viewed().unwrap().len(), 100);
    }
}
