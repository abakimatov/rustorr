use rustorr_domain::{ByteRange, FileIndex, InfoHash, PieceIndex};

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error("torrent {0} is not in the cache")]
    UnknownTorrent(InfoHash),
    #[error("torrent {0} is already open with a different layout")]
    LayoutMismatch(InfoHash),
    #[error("torrent {0} has live views and cannot be removed")]
    Pinned(InfoHash),
    #[error("torrent {torrent} has no file {file:?}")]
    UnknownFile { torrent: InfoHash, file: FileIndex },
    #[error(
        "bytes {range:?} of file {file:?} of torrent {torrent} lie beyond its {file_len} bytes"
    )]
    OutOfBounds {
        torrent: InfoHash,
        file: FileIndex,
        range: ByteRange,
        file_len: u64,
    },
    /// The range was never stored, or its torrent was removed. Never answered
    /// with zeros: see ADR 0004, decision 5.
    #[error("bytes {range:?} of file {file:?} of torrent {torrent} are not stored")]
    Missing {
        torrent: InfoHash,
        file: FileIndex,
        range: ByteRange,
    },
    #[error("piece {piece:?} is out of range, the torrent has {piece_count} pieces")]
    PieceOutOfRange { piece: PieceIndex, piece_count: u32 },
    #[error("invalid torrent layout: {0}")]
    InvalidLayout(&'static str),
    #[error("{context}")]
    Io {
        context: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Domain(#[from] rustorr_domain::Error),
}
