//! Conversions between domain values and what SQLite stores. Every stored
//! value is validated on the way in and again on the way out, so a damaged
//! database surfaces as an error instead of a wrong value.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rustorr_domain::InfoHash;

use crate::Error;

pub(crate) fn unix_seconds(time: SystemTime) -> Result<i64, Error> {
    let seconds = time
        .duration_since(UNIX_EPOCH)
        .map_err(|_| Error::InvalidValue {
            field: "timestamp",
            reason: "is before 1970",
        })?
        .as_secs();
    i64::try_from(seconds).map_err(|_| Error::InvalidValue {
        field: "timestamp",
        reason: "does not fit in 63 bits",
    })
}

pub(crate) fn system_time(seconds: i64) -> Result<SystemTime, Error> {
    let seconds = u64::try_from(seconds).map_err(|_| Error::InvalidValue {
        field: "timestamp",
        reason: "stored value is negative",
    })?;
    Ok(UNIX_EPOCH + Duration::from_secs(seconds))
}

pub(crate) fn info_hash(stored: &[u8]) -> Result<InfoHash, Error> {
    <[u8; 20]>::try_from(stored)
        .map(InfoHash::from_bytes)
        .map_err(|_| Error::InvalidValue {
            field: "info hash",
            reason: "stored value is not 20 bytes",
        })
}
