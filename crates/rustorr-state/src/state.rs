use std::{
    fs,
    path::Path,
    sync::{Mutex, MutexGuard, PoisonError},
    time::Duration,
};

use rusqlite::Connection;

use crate::{
    Error,
    error::database,
    schema::{self, MIGRATIONS},
};

/// Rustorr's persistent state: torrent catalog, settings and viewed history.
///
/// The API is synchronous. One connection is shared behind a mutex, which is
/// plenty for this workload; async callers run it on a blocking thread.
pub struct State {
    connection: Mutex<Connection>,
}

impl State {
    /// Opens the database at `path`, creating it and its parent directories if
    /// needed, and migrates it to the schema this build expects. Refuses files
    /// that are not Rustorr databases and databases from a newer Rustorr.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, Error> {
        Self::open_with(path.as_ref(), MIGRATIONS)
    }

    /// A private in-memory database, gone when the value is dropped.
    pub fn open_in_memory() -> Result<Self, Error> {
        let connection = Connection::open_in_memory().map_err(database("open database"))?;
        Self::initialise(connection, MIGRATIONS)
    }

    pub(crate) fn open_with(path: &Path, migrations: &[&str]) -> Result<Self, Error> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent).map_err(|source| Error::Io {
                path: parent.to_owned(),
                source,
            })?;
        }
        let connection = Connection::open(path).map_err(database("open database"))?;
        Self::initialise(connection, migrations)
    }

    fn initialise(mut connection: Connection, migrations: &[&str]) -> Result<Self, Error> {
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(database("set busy timeout"))?;
        // Checked before anything is changed, so a foreign file is left alone.
        schema::migrate(&mut connection, migrations)?;
        // WAL lets readers proceed during a write. It answers with the mode it
        // ended up in; on a filesystem without shared memory that stays
        // "delete", which is slower but correct, so the answer is not checked.
        connection
            .query_row("PRAGMA journal_mode = WAL", [], |row| {
                row.get::<_, String>(0)
            })
            .map_err(database("enable write-ahead logging"))?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    pub fn schema_version(&self) -> Result<u32, Error> {
        self.connection()
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(database("read schema version"))
    }

    pub(crate) fn connection(&self) -> MutexGuard<'_, Connection> {
        self.connection
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::*;
    use crate::schema::APPLICATION_ID;

    fn db_path(dir: &tempfile::TempDir) -> std::path::PathBuf {
        dir.path().join("state").join("rustorr.db")
    }

    #[test]
    fn state_can_be_shared_between_threads() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<State>();
    }

    #[test]
    fn a_fresh_database_gets_the_latest_schema_and_the_rustorr_application_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = db_path(&dir);

        let state = State::open(&path).unwrap();

        assert_eq!(state.schema_version().unwrap(), MIGRATIONS.len() as u32);
        let application_id: i32 = state
            .connection()
            .pragma_query_value(None, "application_id", |row| row.get(0))
            .unwrap();
        assert_eq!(application_id, APPLICATION_ID);
    }

    #[test]
    fn open_creates_missing_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a").join("b").join("rustorr.db");

        State::open(&path).unwrap();

        assert!(path.is_file());
    }

    #[test]
    fn a_directory_that_cannot_be_created_is_an_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let occupied = dir.path().join("occupied");
        std::fs::write(&occupied, b"").unwrap();

        let error = State::open(occupied.join("rustorr.db")).err().unwrap();

        assert!(matches!(error, Error::Io { .. }), "{error}");
    }

    #[test]
    fn a_file_database_uses_write_ahead_logging() {
        let dir = tempfile::tempdir().unwrap();
        let state = State::open(db_path(&dir)).unwrap();

        let mode: String = state
            .connection()
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();

        assert_eq!(mode, "wal");
    }

    #[test]
    fn opening_twice_keeps_the_version_and_does_not_migrate_again() {
        let dir = tempfile::tempdir().unwrap();
        let path = db_path(&dir);
        drop(State::open(&path).unwrap());

        let state = State::open(&path).unwrap();

        assert_eq!(state.schema_version().unwrap(), MIGRATIONS.len() as u32);
    }

    #[test]
    fn a_database_from_a_newer_rustorr_is_refused_and_left_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = db_path(&dir);
        drop(State::open(&path).unwrap());
        let newer = MIGRATIONS.len() as u32 + 1;
        Connection::open(&path)
            .unwrap()
            .pragma_update(None, "user_version", newer)
            .unwrap();

        let error = State::open(&path).err().unwrap();

        assert!(
            matches!(error, Error::SchemaTooNew { found, supported }
                if found == newer && supported == MIGRATIONS.len() as u32),
            "{error}"
        );
        let still: u32 = Connection::open(&path)
            .unwrap()
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(still, newer);
    }

    #[test]
    fn someone_elses_sqlite_file_is_refused_and_not_switched_to_wal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("other.db");
        {
            let other = Connection::open(&path).unwrap();
            other
                .execute_batch(
                    "CREATE TABLE notes (id INTEGER PRIMARY KEY); INSERT INTO notes VALUES (1);",
                )
                .unwrap();
        }

        let error = State::open(&path).err().unwrap();

        assert!(matches!(error, Error::NotRustorrDatabase), "{error}");
        let other = Connection::open(&path).unwrap();
        let mode: String = other
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_ne!(mode, "wal", "a refused file must not be modified");
        let tables: i64 = other
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tables, 1);
    }

    #[test]
    fn a_versioned_database_with_another_application_id_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("other.db");
        {
            let other = Connection::open(&path).unwrap();
            other.pragma_update(None, "application_id", 42).unwrap();
            other.pragma_update(None, "user_version", 1).unwrap();
        }

        let error = State::open(&path).err().unwrap();

        assert!(matches!(error, Error::NotRustorrDatabase), "{error}");
    }

    #[test]
    fn an_older_database_is_upgraded_step_by_step_and_keeps_its_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = db_path(&dir);
        let v1 = MIGRATIONS[0];
        let v2 = "ALTER TABLE torrents ADD COLUMN note TEXT NOT NULL DEFAULT 'none';";
        let v3 = "CREATE INDEX torrents_by_category ON torrents (category);";
        {
            let state = State::open_with(&path, &[v1]).unwrap();
            state
                .connection()
                .execute(
                    "INSERT INTO torrents (info_hash, added_at, size, metainfo) VALUES (zeroblob(20), 5, 6, x'00ff')",
                    [],
                )
                .unwrap();
        }

        let state = State::open_with(&path, &[v1, v2, v3]).unwrap();

        assert_eq!(state.schema_version().unwrap(), 3);
        let (note, size): (String, i64) = state
            .connection()
            .query_row("SELECT note, size FROM torrents", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        assert_eq!((note.as_str(), size), ("none", 6));
    }

    #[test]
    fn a_failing_migration_rolls_back_and_leaves_the_previous_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = db_path(&dir);
        let v1 = MIGRATIONS[0];
        drop(State::open_with(&path, &[v1]).unwrap());
        let half_applied = "CREATE TABLE extra (id INTEGER); INSERT INTO no_such_table VALUES (1);";

        let error = State::open_with(&path, &[v1, half_applied]).err().unwrap();

        assert!(matches!(error, Error::Database { .. }), "{error}");
        let raw = Connection::open(&path).unwrap();
        let version: u32 = raw
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        let extra: i64 = raw
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name = 'extra'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            (version, extra),
            (1, 0),
            "the failed step must leave no trace"
        );
    }

    #[test]
    fn an_in_memory_database_is_migrated_too() {
        let state = State::open_in_memory().unwrap();

        assert_eq!(state.schema_version().unwrap(), MIGRATIONS.len() as u32);
    }
}
