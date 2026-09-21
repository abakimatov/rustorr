use crate::Error;

/// Half-open range of byte offsets, `start..end`, that may be empty.
///
/// This is a resolved range, not an HTTP `Range` header: suffix, open-ended
/// and multipart forms are resolved against a file length by the HTTP layer
/// before they become a `ByteRange`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ByteRange {
    start: u64,
    end: u64,
}

impl ByteRange {
    pub const fn new(start: u64, end: u64) -> Result<Self, Error> {
        if start > end {
            return Err(Error::RangeReversed { start, end });
        }
        Ok(Self { start, end })
    }

    pub const fn from_start_len(start: u64, len: u64) -> Result<Self, Error> {
        match start.checked_add(len) {
            Some(end) => Ok(Self { start, end }),
            None => Err(Error::RangeOverflow { start, len }),
        }
    }

    pub const fn start(self) -> u64 {
        self.start
    }

    pub const fn end(self) -> u64 {
        self.end
    }

    pub const fn len(self) -> u64 {
        self.end - self.start
    }

    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }

    /// Whether every byte of the range lies inside a file of `length` bytes.
    pub const fn fits_within(self, length: u64) -> bool {
        self.end <= length
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn length_is_end_minus_start() {
        let range = ByteRange::new(10, 25).unwrap();
        assert_eq!((range.start(), range.end(), range.len()), (10, 25, 15));
        assert!(!range.is_empty());
    }

    #[test]
    fn empty_range_is_valid() {
        let range = ByteRange::new(7, 7).unwrap();
        assert_eq!(range.len(), 0);
        assert!(range.is_empty());
    }

    #[test]
    fn reversed_range_is_rejected() {
        assert_eq!(
            ByteRange::new(6, 5),
            Err(Error::RangeReversed { start: 6, end: 5 })
        );
    }

    #[test]
    fn start_and_length_agree_with_start_and_end() {
        assert_eq!(
            ByteRange::from_start_len(10, 15).unwrap(),
            ByteRange::new(10, 25).unwrap()
        );
    }

    #[test]
    fn length_that_overflows_is_rejected() {
        assert_eq!(
            ByteRange::from_start_len(u64::MAX, 1),
            Err(Error::RangeOverflow {
                start: u64::MAX,
                len: 1
            })
        );
        let last = ByteRange::from_start_len(u64::MAX - 1, 1).unwrap();
        assert_eq!(last.end(), u64::MAX);
    }

    #[test]
    fn range_ending_exactly_at_the_file_length_fits() {
        let range = ByteRange::new(90, 100).unwrap();
        assert!(range.fits_within(100));
        assert!(!range.fits_within(99));
    }

    #[test]
    fn empty_range_past_the_end_does_not_fit() {
        assert!(ByteRange::new(100, 100).unwrap().fits_within(100));
        assert!(!ByteRange::new(101, 101).unwrap().fits_within(100));
    }
}
