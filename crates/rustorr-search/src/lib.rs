//! MatriX.145's search integrations: the Rutor database and Torznab
//! indexers. Neither is part of the client core; the HTTP layer reaches them
//! through [`Search`].

mod rutor;
mod service;
mod torznab;

use serde::{Deserialize, Serialize};

pub use rutor::RutorDatabase;
pub use service::{RUTOR_URL, Search, SearchFuture, SearchService};
pub use torznab::{CategoryType, Indexer};

/// One search result, field for field as MatriX.145 writes it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TorrentDetails {
    #[serde(rename = "Title")]
    pub title: String,
    #[serde(rename = "Name")]
    pub name: String,
    /// `null` when the source had none, as Go writes a nil slice.
    #[serde(rename = "Names")]
    pub names: Option<Vec<String>>,
    #[serde(rename = "Categories")]
    pub categories: String,
    #[serde(rename = "Size")]
    pub size: String,
    /// RFC 3339, kept as Go wrote it.
    #[serde(rename = "CreateDate")]
    pub create_date: String,
    #[serde(rename = "Tracker")]
    pub tracker: String,
    #[serde(rename = "Link")]
    pub link: String,
    #[serde(rename = "Year")]
    pub year: i64,
    #[serde(rename = "Peer")]
    pub peer: i64,
    #[serde(rename = "Seed")]
    pub seed: i64,
    #[serde(rename = "Magnet")]
    pub magnet: String,
    #[serde(rename = "Hash")]
    pub hash: String,
    #[serde(rename = "IMDBID")]
    pub imdb_id: String,
    #[serde(rename = "VideoQuality")]
    pub video_quality: i64,
    #[serde(rename = "AudioQuality")]
    pub audio_quality: i64,
}

impl TorrentDetails {
    fn names_joined(&self) -> String {
        self.names.as_deref().unwrap_or_default().join(" ")
    }
}
