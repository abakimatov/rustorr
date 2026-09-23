#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteRange {
    pub start: u64,
    pub end: u64,
}

impl ByteRange {
    pub fn len(self) -> u64 {
        self.end - self.start + 1
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeError {
    Invalid,
    Unsatisfiable,
}

pub fn parse(value: &str, length: u64) -> Result<Vec<ByteRange>, RangeError> {
    let values = value.strip_prefix("bytes=").ok_or(RangeError::Invalid)?;
    if length == 0 {
        return Err(RangeError::Unsatisfiable);
    }
    let mut ranges = Vec::new();
    for value in values.split(',') {
        let (start, end) = value.trim().split_once('-').ok_or(RangeError::Invalid)?;
        let range = if start.is_empty() {
            let suffix: u64 = end.parse().map_err(|_| RangeError::Invalid)?;
            if suffix == 0 {
                return Err(RangeError::Unsatisfiable);
            }
            ByteRange {
                start: length.saturating_sub(suffix),
                end: length - 1,
            }
        } else {
            let start: u64 = start.parse().map_err(|_| RangeError::Invalid)?;
            if start >= length {
                return Err(RangeError::Unsatisfiable);
            }
            let end = if end.is_empty() {
                length - 1
            } else {
                end.parse::<u64>()
                    .map_err(|_| RangeError::Invalid)?
                    .min(length - 1)
            };
            if end < start {
                return Err(RangeError::Unsatisfiable);
            }
            ByteRange { start, end }
        };
        ranges.push(range);
        if ranges.len() > 64 {
            return Err(RangeError::Invalid);
        }
    }
    if ranges.is_empty() {
        Err(RangeError::Invalid)
    } else {
        Ok(ranges)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_closed_open_suffix_and_multiple_ranges() {
        assert_eq!(
            parse("bytes=2-4", 10).unwrap(),
            [ByteRange { start: 2, end: 4 }]
        );
        assert_eq!(
            parse("bytes=7-", 10).unwrap(),
            [ByteRange { start: 7, end: 9 }]
        );
        assert_eq!(
            parse("bytes=-3", 10).unwrap(),
            [ByteRange { start: 7, end: 9 }]
        );
        assert_eq!(
            parse("bytes=0-1,8-99", 10).unwrap(),
            [
                ByteRange { start: 0, end: 1 },
                ByteRange { start: 8, end: 9 }
            ]
        );
    }

    #[test]
    fn rejects_malformed_and_unsatisfiable_ranges() {
        assert_eq!(parse("items=0-1", 10), Err(RangeError::Invalid));
        assert_eq!(parse("bytes=10-", 10), Err(RangeError::Unsatisfiable));
        assert_eq!(parse("bytes=-0", 10), Err(RangeError::Unsatisfiable));
    }
}
