use rustorr_domain::{ByteRange, FileIndex, InfoHash, PieceIndex};

use crate::Error;

/// Shape of a torrent's data: what the cache needs to know to place bytes and
/// to size its piece index. Built by the engine adapter from torrent metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TorrentLayout {
    piece_length: u64,
    file_lengths: Vec<u64>,
    total_length: u64,
    piece_count: u32,
}

impl TorrentLayout {
    pub fn new(piece_length: u64, file_lengths: Vec<u64>) -> Result<Self, Error> {
        if piece_length == 0 {
            return Err(Error::InvalidLayout("piece length is zero"));
        }
        let total_length = file_lengths
            .iter()
            .try_fold(0u64, |total, &length| total.checked_add(length))
            .ok_or(Error::InvalidLayout("total length overflows"))?;
        let piece_count = u32::try_from(total_length.div_ceil(piece_length))
            .map_err(|_| Error::InvalidLayout("more pieces than fit in 32 bits"))?;
        Ok(Self {
            piece_length,
            file_lengths,
            total_length,
            piece_count,
        })
    }

    pub fn piece_length(&self) -> u64 {
        self.piece_length
    }

    pub fn total_length(&self) -> u64 {
        self.total_length
    }

    pub fn piece_count(&self) -> u32 {
        self.piece_count
    }

    pub fn file_count(&self) -> usize {
        self.file_lengths.len()
    }

    pub fn file_length(&self, file: FileIndex) -> Option<u64> {
        self.file_lengths.get(file.zero_based() as usize).copied()
    }

    /// Validates that `len` bytes at `offset` lie inside `file`.
    pub(crate) fn check_range(
        &self,
        torrent: InfoHash,
        file: FileIndex,
        offset: u64,
        len: usize,
    ) -> Result<ByteRange, Error> {
        let file_len = self
            .file_length(file)
            .ok_or(Error::UnknownFile { torrent, file })?;
        let range = ByteRange::from_start_len(offset, len as u64)?;
        if !range.fits_within(file_len) {
            return Err(Error::OutOfBounds {
                torrent,
                file,
                range,
                file_len,
            });
        }
        Ok(range)
    }
}

/// Pieces of one torrent that the engine has completed and verified.
#[derive(Debug)]
pub(crate) struct PieceSet {
    complete: Vec<bool>,
}

impl PieceSet {
    pub fn new(piece_count: u32) -> Self {
        Self {
            complete: vec![false; piece_count as usize],
        }
    }

    pub fn insert(&mut self, piece: PieceIndex) -> Result<(), Error> {
        match self.complete.get_mut(piece.get() as usize) {
            Some(slot) => {
                *slot = true;
                Ok(())
            }
            None => Err(Error::PieceOutOfRange {
                piece,
                piece_count: self.complete.len() as u32,
            }),
        }
    }

    pub fn contains(&self, piece: PieceIndex) -> bool {
        self.complete
            .get(piece.get() as usize)
            .copied()
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn torrent() -> InfoHash {
        InfoHash::from_bytes([1; 20])
    }

    fn file(index: u32) -> FileIndex {
        FileIndex::from_zero_based(index)
    }

    #[test]
    fn counts_pieces_including_a_short_last_one() {
        let layout = TorrentLayout::new(100, vec![150, 60]).unwrap();
        assert_eq!(layout.total_length(), 210);
        assert_eq!(layout.piece_count(), 3);
        assert_eq!(layout.file_count(), 2);
        assert_eq!(layout.file_length(file(1)), Some(60));
        assert_eq!(layout.file_length(file(2)), None);
    }

    #[test]
    fn rejects_unusable_layouts() {
        assert!(matches!(
            TorrentLayout::new(0, vec![1]),
            Err(Error::InvalidLayout(_))
        ));
        assert!(matches!(
            TorrentLayout::new(1, vec![u64::MAX, 1]),
            Err(Error::InvalidLayout(_))
        ));
        assert!(matches!(
            TorrentLayout::new(1, vec![u64::from(u32::MAX) + 1]),
            Err(Error::InvalidLayout(_))
        ));
    }

    #[test]
    fn empty_files_are_valid() {
        let layout = TorrentLayout::new(16, vec![0, 32, 0]).unwrap();
        assert_eq!(layout.piece_count(), 2);
        assert!(layout.check_range(torrent(), file(0), 0, 0).is_ok());
    }

    #[test]
    fn range_check_accepts_up_to_the_end_of_the_file() {
        let layout = TorrentLayout::new(16, vec![100]).unwrap();
        assert!(layout.check_range(torrent(), file(0), 90, 10).is_ok());
        assert!(matches!(
            layout.check_range(torrent(), file(0), 90, 11),
            Err(Error::OutOfBounds { file_len: 100, .. })
        ));
    }

    #[test]
    fn range_check_rejects_unknown_files_and_overflow() {
        let layout = TorrentLayout::new(16, vec![100]).unwrap();
        assert!(matches!(
            layout.check_range(torrent(), file(1), 0, 1),
            Err(Error::UnknownFile { .. })
        ));
        assert!(matches!(
            layout.check_range(torrent(), file(0), u64::MAX, 1),
            Err(Error::Domain(_))
        ));
    }

    #[test]
    fn piece_set_tracks_completion_and_bounds() {
        let mut pieces = PieceSet::new(3);
        assert!(!pieces.contains(PieceIndex::new(1)));
        pieces.insert(PieceIndex::new(1)).unwrap();
        assert!(pieces.contains(PieceIndex::new(1)));
        assert!(!pieces.contains(PieceIndex::new(0)));
        assert!(matches!(
            pieces.insert(PieceIndex::new(3)),
            Err(Error::PieceOutOfRange { piece_count: 3, .. })
        ));
        assert!(!pieces.contains(PieceIndex::new(3)));
    }
}
