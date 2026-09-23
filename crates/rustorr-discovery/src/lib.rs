//! Local network discovery MatriX.145 offers: Bonjour (mDNS/DNS-SD) for the
//! web server and SSDP for the DLNA media server, plus the names and device
//! identifiers both announce. No HTTP here: the DLNA device description and
//! ContentDirectory are served by `rustorr-http`.

mod bonjour;
mod dns;
pub mod identity;
pub mod interfaces;
mod socket;
mod ssdp;

pub use bonjour::{Bonjour, BonjourConfig};
pub use ssdp::{DEVICE_TYPE, SERVER, SERVICE_TYPES, Ssdp, SsdpConfig};
