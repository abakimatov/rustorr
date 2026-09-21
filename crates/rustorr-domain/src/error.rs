/// Validation failures for domain values built from untrusted input.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error("info hash must be {expected} hex characters, got {actual}")]
    InfoHashLength { expected: usize, actual: usize },
    #[error("info hash has non-hex character {character:?} at position {position}")]
    InfoHashCharacter { position: usize, character: char },
    #[error("file index is counted from 1; 0 is not a valid index")]
    FileIndexZero,
    #[error("byte range starts at {start} but ends at {end}")]
    RangeReversed { start: u64, end: u64 },
    #[error("byte range of {len} bytes from {start} overflows")]
    RangeOverflow { start: u64, len: u64 },
}
