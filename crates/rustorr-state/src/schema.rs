use rusqlite::{Connection, TransactionBehavior};

use crate::{Error, error::database};

/// Stored in `PRAGMA application_id`: "RSTR". Lets `open` refuse someone
/// else's SQLite file instead of writing into it.
pub(crate) const APPLICATION_ID: i32 = 0x5253_5452;

/// Migrations in order; the position plus one is the schema version, kept in
/// `PRAGMA user_version`. Applied migrations are never edited: a change to the
/// schema is a new entry.
pub(crate) const MIGRATIONS: &[&str] = &[V1, V2];

const V1: &str = "
PRAGMA application_id = 1381192786; -- APPLICATION_ID, in decimal

-- Catalog of saved torrents. `metainfo` is kept apart from the listing columns
-- so that listing does not read it. `added_at` is unix seconds.
CREATE TABLE torrents (
    info_hash BLOB    PRIMARY KEY CHECK (length(info_hash) = 20),
    title     TEXT    NOT NULL DEFAULT '',
    poster    TEXT    NOT NULL DEFAULT '',
    category  TEXT    NOT NULL DEFAULT '',
    data      TEXT    NOT NULL DEFAULT '',
    added_at  INTEGER NOT NULL,
    size      INTEGER NOT NULL CHECK (size >= 0),
    metainfo  BLOB    NOT NULL
) STRICT;

-- One JSON document, replaced as a whole. Not tied to the catalog.
CREATE TABLE settings (
    id       INTEGER PRIMARY KEY CHECK (id = 1),
    document TEXT    NOT NULL CHECK (json_valid(document))
) STRICT;

-- Viewed files and playback position. Deliberately not tied to the catalog:
-- the reference keeps them separately. `file_index` is zero-based.
CREATE TABLE viewed (
    info_hash  BLOB    NOT NULL CHECK (length(info_hash) = 20),
    file_index INTEGER NOT NULL CHECK (file_index >= 0),
    timecode   REAL    NOT NULL DEFAULT 0,
    PRIMARY KEY (info_hash, file_index)
) STRICT, WITHOUT ROWID;
";

const V2: &str = "
-- WAF configuration is normal application state. Account credentials are
-- intentionally not here: accs.db remains a deployment secret.
CREATE TABLE waf (
    id        INTEGER PRIMARY KEY CHECK (id = 1),
    whitelist TEXT NOT NULL DEFAULT '',
    blacklist TEXT NOT NULL DEFAULT '',
    referers  TEXT NOT NULL DEFAULT ''
) STRICT;
";

fn user_version(connection: &Connection) -> Result<u32, Error> {
    connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(database("read schema version"))
}

/// Brings the database to the latest version, one transaction per migration,
/// so a failing migration leaves the previous version intact.
pub(crate) fn migrate(connection: &mut Connection, migrations: &[&str]) -> Result<(), Error> {
    let latest = migrations.len() as u32;
    let found = user_version(connection)?;

    if found == 0 {
        let tables: i64 = connection
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'table'",
                [],
                |row| row.get(0),
            )
            .map_err(database("inspect database"))?;
        if tables > 0 {
            return Err(Error::NotRustorrDatabase);
        }
    } else {
        let application_id: i32 = connection
            .pragma_query_value(None, "application_id", |row| row.get(0))
            .map_err(database("read application id"))?;
        if application_id != APPLICATION_ID {
            return Err(Error::NotRustorrDatabase);
        }
    }
    if found > latest {
        return Err(Error::SchemaTooNew {
            found,
            supported: latest,
        });
    }

    for target in found + 1..=latest {
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(database("begin migration"))?;
        // Another process may have applied this step while we waited.
        if user_version(&transaction)? >= target {
            continue;
        }
        transaction
            .execute_batch(migrations[target as usize - 1])
            .map_err(database("apply migration"))?;
        transaction
            .pragma_update(None, "user_version", target)
            .map_err(database("record schema version"))?;
        transaction.commit().map_err(database("commit migration"))?;
    }
    Ok(())
}
