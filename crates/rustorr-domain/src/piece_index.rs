/// Zero-based position of a piece within a torrent.
///
/// Whether the index is in bounds depends on the torrent's piece count, which
/// only the engine knows, so this type does not validate it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PieceIndex(u32);

impl PieceIndex {
    pub const fn new(index: u32) -> Self {
        Self(index)
    }

    pub const fn get(self) -> u32 {
        self.0
    }
}
