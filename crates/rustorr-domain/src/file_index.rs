use crate::Error;

/// Position of a file within a torrent.
///
/// Stored zero-based, the way the engine counts. TorrServer's API counts from
/// one (`file_stats[].id` and the `index` query parameter), so every crossing
/// of that boundary goes through `from_one_based` / `one_based`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FileIndex(u32);

impl FileIndex {
    pub const fn from_zero_based(index: u32) -> Self {
        Self(index)
    }

    pub const fn from_one_based(index: u32) -> Result<Self, Error> {
        match index.checked_sub(1) {
            Some(zero_based) => Ok(Self(zero_based)),
            None => Err(Error::FileIndexZero),
        }
    }

    pub const fn zero_based(self) -> u32 {
        self.0
    }

    /// Wider than `u32` because the last zero-based index plus one does not fit.
    pub const fn one_based(self) -> u64 {
        self.0 as u64 + 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_file_is_one_in_the_api_and_zero_in_the_engine() {
        let index = FileIndex::from_one_based(1).unwrap();
        assert_eq!(index.zero_based(), 0);
        assert_eq!(index.one_based(), 1);
        assert_eq!(index, FileIndex::from_zero_based(0));
    }

    #[test]
    fn zero_is_not_a_valid_api_index() {
        assert_eq!(FileIndex::from_one_based(0), Err(Error::FileIndexZero));
    }

    #[test]
    fn conversions_round_trip_at_the_extremes() {
        let last = FileIndex::from_one_based(u32::MAX).unwrap();
        assert_eq!(last.zero_based(), u32::MAX - 1);
        assert_eq!(last.one_based(), u64::from(u32::MAX));

        let beyond = FileIndex::from_zero_based(u32::MAX);
        assert_eq!(beyond.one_based(), u64::from(u32::MAX) + 1);
    }

    #[test]
    fn orders_by_position() {
        assert!(FileIndex::from_zero_based(1) < FileIndex::from_zero_based(2));
    }
}
