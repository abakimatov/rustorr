use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TorznabConfig {
    #[serde(rename = "Host")]
    pub host: String,
    #[serde(rename = "Key")]
    pub key: String,
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "Categories")]
    pub categories: String,
    #[serde(rename = "CatType")]
    pub category_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TmdbConfig {
    #[serde(rename = "APIKey")]
    pub api_key: String,
    #[serde(rename = "APIURL")]
    pub api_url: String,
    #[serde(rename = "ImageURL")]
    pub image_url: String,
    #[serde(rename = "ImageURLRu")]
    pub image_url_ru: String,
}

impl Default for TmdbConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            api_url: "https://api.themoviedb.org".into(),
            image_url: "https://image.tmdb.org".into(),
            image_url_ru: "https://imagetmdb.com".into(),
        }
    }
}

/// The complete MatriX.145 settings document. Some fields are deliberately
/// persistence-only until their owning R7/R9 stage; preserving them is still
/// part of the R6 wire contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    #[serde(rename = "CacheSize")]
    pub cache_size: i64,
    #[serde(rename = "ReaderReadAHead")]
    pub reader_read_ahead: i32,
    #[serde(rename = "PreloadCache")]
    pub preload_cache: i32,
    #[serde(rename = "UseDisk")]
    pub use_disk: bool,
    #[serde(rename = "TorrentsSavePath")]
    pub torrents_save_path: String,
    #[serde(rename = "RemoveCacheOnDrop")]
    pub remove_cache_on_drop: bool,
    #[serde(rename = "ForceEncrypt")]
    pub force_encrypt: bool,
    #[serde(rename = "RetrackersMode")]
    pub retrackers_mode: i32,
    #[serde(rename = "TrackersListURL")]
    pub trackers_list_url: String,
    #[serde(rename = "DefaultTrackers")]
    pub default_trackers: String,
    #[serde(rename = "TorrentDisconnectTimeout")]
    pub torrent_disconnect_timeout: i32,
    #[serde(rename = "EnableDebug")]
    pub enable_debug: bool,
    #[serde(rename = "EnableDLNA")]
    pub enable_dlna: bool,
    #[serde(rename = "EnableBonjour")]
    pub enable_bonjour: bool,
    #[serde(rename = "FriendlyName")]
    pub friendly_name: String,
    #[serde(rename = "EnableRutorSearch")]
    pub enable_rutor_search: bool,
    #[serde(rename = "EnableTorznabSearch")]
    pub enable_torznab_search: bool,
    #[serde(rename = "TorznabUrls")]
    pub torznab_urls: Option<Vec<TorznabConfig>>,
    #[serde(rename = "TMDBSettings")]
    pub tmdb_settings: TmdbConfig,
    #[serde(rename = "EnableIPv6")]
    pub enable_ipv6: bool,
    #[serde(rename = "DisableTCP")]
    pub disable_tcp: bool,
    #[serde(rename = "DisableUTP")]
    pub disable_utp: bool,
    #[serde(rename = "DisableUPNP")]
    pub disable_upnp: bool,
    #[serde(rename = "DisableDHT")]
    pub disable_dht: bool,
    #[serde(rename = "DisablePEX")]
    pub disable_pex: bool,
    #[serde(rename = "DisableUpload")]
    pub disable_upload: bool,
    #[serde(rename = "DownloadRateLimit")]
    pub download_rate_limit: i32,
    #[serde(rename = "UploadRateLimit")]
    pub upload_rate_limit: i32,
    #[serde(rename = "ConnectionsLimit")]
    pub connections_limit: i32,
    #[serde(rename = "PeersListenPort")]
    pub peers_listen_port: i32,
    #[serde(rename = "EnableLPD")]
    pub enable_lpd: bool,
    #[serde(rename = "LPDIPv6")]
    pub lpd_ipv6: bool,
    #[serde(rename = "SslPort")]
    pub ssl_port: i32,
    #[serde(rename = "SslCert")]
    pub ssl_cert: String,
    #[serde(rename = "SslKey")]
    pub ssl_key: String,
    #[serde(rename = "ResponsiveMode")]
    pub responsive_mode: bool,
    #[serde(rename = "ShowFSActiveTorr")]
    pub show_fs_active_torr: bool,
    #[serde(rename = "StoreSettingsInJson")]
    pub store_settings_in_json: bool,
    #[serde(rename = "StoreViewedInJson")]
    pub store_viewed_in_json: bool,
    #[serde(rename = "TrackTimecode")]
    pub track_timecode: bool,
    #[serde(rename = "MergeAllM3U")]
    pub merge_all_m3u: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            cache_size: 64 << 20,
            reader_read_ahead: 95,
            preload_cache: 50,
            use_disk: false,
            torrents_save_path: String::new(),
            remove_cache_on_drop: false,
            force_encrypt: false,
            retrackers_mode: 1,
            trackers_list_url: String::new(),
            default_trackers: "http://retracker.local/announce\nhttp://bt4.t-ru.org/ann?magnet\nhttp://retracker.mgts.by:80/announce\nhttp://tracker.city9x.com:2710/announce\nhttp://tracker.electro-torrent.pl:80/announce\nhttp://tracker.internetwarriors.net:1337/announce\nhttp://tracker2.itzmx.com:6961/announce\nudp://opentor.org:2710\nudp://public.popcorn-tracker.org:6969/announce\nudp://tracker.opentrackr.org:1337/announce\nhttp://bt.svao-ix.ru/announce\nudp://explodie.org:6969/announce\nwss://tracker.btorrent.xyz\nwss://tracker.openwebtorrent.com".into(),
            torrent_disconnect_timeout: 30,
            enable_debug: false,
            enable_dlna: false,
            enable_bonjour: true,
            friendly_name: String::new(),
            enable_rutor_search: false,
            enable_torznab_search: false,
            torznab_urls: None,
            tmdb_settings: TmdbConfig::default(),
            enable_ipv6: false,
            disable_tcp: false,
            disable_utp: false,
            disable_upnp: false,
            disable_dht: false,
            disable_pex: false,
            disable_upload: false,
            download_rate_limit: 0,
            upload_rate_limit: 0,
            connections_limit: 25,
            peers_listen_port: 0,
            enable_lpd: true,
            lpd_ipv6: false,
            ssl_port: 0,
            ssl_cert: String::new(),
            ssl_key: String::new(),
            responsive_mode: true,
            show_fs_active_torr: true,
            store_settings_in_json: true,
            store_viewed_in_json: false,
            track_timecode: false,
            merge_all_m3u: false,
        }
    }
}

impl Settings {
    pub fn normalized(mut self) -> Self {
        if self.cache_size == 0 {
            self.cache_size = 64 << 20;
        }
        if self.connections_limit == 0 {
            self.connections_limit = 25;
        }
        if self.torrent_disconnect_timeout == 0 {
            self.torrent_disconnect_timeout = 30;
        }
        self.reader_read_ahead = self.reader_read_ahead.clamp(5, 100);
        self.preload_cache = self.preload_cache.clamp(0, 100);
        if self.torrents_save_path.is_empty() {
            self.use_disk = false;
        }
        self
    }

    pub fn cache_cap(&self) -> u64 {
        u64::try_from(self.cache_size).unwrap_or(0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TorrentFileView {
    pub id: u32,
    pub path: String,
    pub length: u64,
    #[serde(skip)]
    pub engine_index: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TorrentView {
    pub title: String,
    pub category: String,
    pub poster: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    pub timestamp: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub torrs_hash: Option<String>,
    pub stat: u8,
    pub stat_string: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loaded_size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub torrent_size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub download_speed: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upload_speed: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_peers: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_peers: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connected_seeders: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes_written: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes_read: Option<u64>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub file_stats: Vec<TorrentFileView>,
}

impl TorrentView {
    pub fn hash(&self) -> Option<rustorr_domain::InfoHash> {
        self.hash.as_deref()?.parse().ok()
    }
}

pub(crate) fn unix_seconds(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_the_complete_reference_document() {
        let value = serde_json::to_value(Settings::default()).unwrap();
        assert_eq!(value.as_object().unwrap().len(), 41);
        assert_eq!(value["CacheSize"], 67_108_864);
        assert_eq!(value["TorznabUrls"], serde_json::Value::Null);
        assert_eq!(value["TMDBSettings"]["ImageURLRu"], "https://imagetmdb.com");
    }

    #[test]
    fn reference_clamps_are_applied_once() {
        let settings = Settings {
            cache_size: 0,
            connections_limit: 0,
            torrent_disconnect_timeout: 0,
            reader_read_ahead: -1,
            preload_cache: 200,
            use_disk: true,
            ..Settings::default()
        }
        .normalized();
        assert_eq!(settings.cache_size, 64 << 20);
        assert_eq!(settings.connections_limit, 25);
        assert_eq!(settings.torrent_disconnect_timeout, 30);
        assert_eq!(settings.reader_read_ahead, 5);
        assert_eq!(settings.preload_cache, 100);
        assert!(!settings.use_disk);
    }
}
