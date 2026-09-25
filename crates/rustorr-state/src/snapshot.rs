//! Consistent copies of a database file for backups and the checks a restore
//! needs, without opening the file as `State` (which would migrate it).

use std::path::Path;

use rusqlite::{Connection, OpenFlags};

use crate::{
    Error,
    error::database,
    schema::{self, MIGRATIONS},
};

/// The schema version this build creates and understands.
pub const SCHEMA_VERSION: u32 = MIGRATIONS.len() as u32;

fn open_read_only(path: &Path) -> Result<Connection, Error> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(database("open database"))?;
    connection
        .busy_timeout(std::time::Duration::from_secs(5))
        .map_err(database("set busy timeout"))?;
    Ok(connection)
}

/// The schema version of the Rustorr database at `path`, checked as
/// `State::open` would check it and left unchanged.
pub fn inspect(path: &Path) -> Result<u32, Error> {
    schema::check(&open_read_only(path)?, SCHEMA_VERSION)
}

/// Writes a consistent copy of the database at `source` to `target`, which
/// must not exist, with `VACUUM INTO`: a server may keep writing meanwhile.
/// Returns the copy's schema version.
pub fn snapshot(source: &Path, target: &Path) -> Result<u32, Error> {
    let connection = open_read_only(source)?;
    let version = schema::check(&connection, SCHEMA_VERSION)?;
    let target = target.to_str().ok_or(Error::InvalidValue {
        field: "backup path",
        reason: "not valid UTF-8",
    })?;
    connection
        .execute("VACUUM INTO ?1", [target])
        .map_err(database("copy database"))?;
    Ok(version)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CatalogEntry, State};

    #[test]
    fn a_snapshot_holds_the_data_of_a_database_in_use() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rustorr.db");
        let state = State::open(&path).unwrap();
        let hash = "d272ca49e3f32a0a08c0c0599a0a9daa6bf5cb7d".parse().unwrap();
        state
            .save_torrent(
                &CatalogEntry {
                    hash,
                    title: "Медиа".into(),
                    poster: String::new(),
                    category: "movie".into(),
                    data: String::new(),
                    added_at: std::time::SystemTime::UNIX_EPOCH,
                    size: 1,
                },
                b"d4:infode",
            )
            .unwrap();

        let copy = dir.path().join("copy.db");
        assert_eq!(snapshot(&path, &copy).unwrap(), SCHEMA_VERSION);
        assert_eq!(inspect(&copy).unwrap(), SCHEMA_VERSION);
        let restored = State::open(&copy).unwrap();
        assert_eq!(restored.torrent(hash).unwrap().unwrap().title, "Медиа");
        // The source stays usable.
        assert!(state.torrent(hash).unwrap().is_some());
    }

    #[test]
    fn foreign_and_newer_files_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let foreign = dir.path().join("foreign.db");
        Connection::open(&foreign)
            .unwrap()
            .execute_batch("CREATE TABLE t (x)")
            .unwrap();
        assert!(matches!(inspect(&foreign), Err(Error::NotRustorrDatabase)));

        let newer = dir.path().join("newer.db");
        State::open(&newer).unwrap();
        Connection::open(&newer)
            .unwrap()
            .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .unwrap();
        assert!(matches!(
            snapshot(&newer, &dir.path().join("x.db")),
            Err(Error::SchemaTooNew { .. })
        ));
    }
}
