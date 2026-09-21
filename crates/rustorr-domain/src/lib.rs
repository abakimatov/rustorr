//! Domain types shared by every Rustorr crate: identifiers, byte ranges and the
//! error vocabulary. No IO and no dependency on any other workspace crate.
//!
//! Error conventions for the workspace:
//! - each library crate exports one `Error` enum built with `thiserror`,
//!   marked `#[non_exhaustive]`, and may wrap this crate's `Error` via `#[from]`;
//! - libraries never return `anyhow::Error`; only the `rustorr` binary does;
//! - engine and storage error types are converted inside their own crate and
//!   never appear in another crate's signatures.

mod byte_range;
mod error;
mod file_index;
mod info_hash;
mod piece_index;

pub use byte_range::ByteRange;
pub use error::Error;
pub use file_index::FileIndex;
pub use info_hash::InfoHash;
pub use piece_index::PieceIndex;
