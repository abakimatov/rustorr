//! HTTP surface: router, error-to-response mapping and the layers every
//! request passes through. The only place a domain error becomes an HTTP
//! status and body.
//!
//! Auth, WAF and CORS (R6) plug in as tower layers inside `app::with_layers`.

mod access;
mod access_log;
mod app;
mod discovery;
mod dlna;
mod error;
mod ffprobe_api;
mod file_server;
mod go_decode;
mod gstreamer_api;
mod listeners;
mod m3u;
mod media_type;
mod msx_api;
mod range;
mod search_api;
mod serve_content;
mod settings_api;
mod web_api;
mod web_ui;
mod webdav;

pub use access::{Credentials, HttpConfig};
pub use access_log::AccessLog;
pub use app::{
    Integrations, ServerInfo, router, router_with_core, router_with_lifecycle,
    router_with_services, serve, serve_with_core, serve_with_lifecycle, serve_with_services,
};
pub use discovery::{Discovery, DiscoveryChange, DiscoveryFuture};
pub use dlna::{DlnaDevice, dlna_router, serve_dlna};
pub use error::ApiError;
pub use ffprobe_api::locate_ffprobe;
pub use gstreamer_api::GstreamerSetup;
pub use listeners::Listeners;
pub use msx_api::{MSX_LANDING_URL, Msx};
