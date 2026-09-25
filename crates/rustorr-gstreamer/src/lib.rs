//! MatriX.145's GStreamer module (`server/gstreamer`, built with `-tags
//! gst`): Matroska files remuxed, or transcoded, into HLS with fragmented
//! MP4 segments and WebVTT subtitles.
//!
//! The playlists, the MP4 repackaging, the cue timeline and the subtitle
//! store are plain Rust and always built. The pipeline itself needs
//! GStreamer: with the `runtime` feature it runs on gstreamer-rs, without it
//! the runtime reports itself unavailable.

mod config;
pub mod cue;
pub mod env;
pub mod mp4box;
pub mod mp4init;
#[cfg(feature = "runtime")]
pub mod pipeline;
pub mod playlist;
pub mod probe;
pub mod service;
pub mod subtitles;

pub use config::{Config, MIN_GST_VERSION};

/// The reference's error values; their texts reach clients in `502` bodies.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("bad gstreamer source")]
    BadSource,
    #[error("unsupported container; only Matroska/WebM is supported: {0}")]
    UnsupportedContainer(String),
    #[error("unsupported video codec")]
    UnsupportedVideo,
    #[error("HDR tone mapping requires a PQ or HLG base layer")]
    UnsupportedHdrTransfer,
    #[error("gst-discoverer returned no stream info")]
    ProbeUnavailable,
    /// `errors.Join(ErrPipelineUnavailable, cause)`.
    #[error("gstreamer runtime is unavailable\n{0}")]
    PipelineUnavailable(String),
    #[error("segment is not ready")]
    SegmentNotReady,
    #[error("gstreamer task not found")]
    TaskNotFound,
    #[error("gstreamer service is closed")]
    ServiceClosed,
    #[error("invalid gstreamer task id")]
    InvalidIdentifier,
    /// Carries the `: position=… threshold=… duration=…` detail.
    #[error("gstreamer reached EOS before the expected end{0}")]
    EarlyEndOfStream(String),
    #[error("gstreamer end of stream is exhausted")]
    EndOfStreamExhausted,
    #[error("truncated mp4 fragment at end of stream: {0}")]
    TruncatedMp4Fragment(String),
    #[error("undecodable mp4 eos remainder: {0}")]
    UndecodableEosRemainder(String),
    /// `context.DeadlineExceeded`: the probe outlived its timeout.
    #[error("context deadline exceeded")]
    DeadlineExceeded,
    #[error("context canceled")]
    Canceled,
    #[error("{0}")]
    Other(String),
}
