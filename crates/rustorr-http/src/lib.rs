//! HTTP surface: router, error-to-response mapping and the layers every
//! request passes through. The only place a domain error becomes an HTTP
//! status and body.
//!
//! Auth, WAF and CORS (R6) plug in as tower layers inside `app::with_layers`.

mod access;
mod app;
mod error;
mod m3u;
mod range;
mod settings_api;
mod web_api;

pub use access::{Credentials, HttpConfig};
pub use app::{
    ServerInfo, router, router_with_core, router_with_lifecycle, serve, serve_with_core,
    serve_with_lifecycle,
};
pub use error::ApiError;
