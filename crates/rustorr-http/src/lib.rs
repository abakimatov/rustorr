//! HTTP surface: router, error-to-response mapping and the layers every
//! request passes through. The only place a domain error becomes an HTTP
//! status and body.
//!
//! Auth, WAF and CORS (R6) plug in as tower layers inside `app::with_layers`.

mod app;
mod error;

pub use app::{ServerInfo, router, serve};
pub use error::ApiError;
