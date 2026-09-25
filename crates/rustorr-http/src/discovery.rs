//! The port through which API changes reach local network discovery
//! (Bonjour and the DLNA media server), which the server wires up.

use std::{future::Future, pin::Pin};

use rustorr_lifecycle::Settings;

pub type DiscoveryFuture<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

/// What a `/settings` change asks of discovery.
#[derive(Debug, Clone)]
pub enum DiscoveryChange {
    /// `set`: MatriX.145 restarts each service the request enables, reading
    /// names from the settings now in effect (unchanged in read-only mode).
    Set {
        dlna: bool,
        bonjour: bool,
        settings: Box<Settings>,
    },
    /// `def`: both services stop, whatever the defaults say.
    Defaults,
}

pub trait Discovery: Send + Sync {
    fn settings_changed(&self, change: DiscoveryChange) -> DiscoveryFuture<'_>;
    /// After a torrent is added or removed, or the catalog wiped: a running
    /// DLNA server restarts.
    fn catalog_changed(&self) -> DiscoveryFuture<'_>;
}

/// Stands in when no discovery is wired, as in router tests.
pub(crate) struct NoDiscovery;

impl Discovery for NoDiscovery {
    fn settings_changed(&self, _change: DiscoveryChange) -> DiscoveryFuture<'_> {
        Box::pin(async {})
    }

    fn catalog_changed(&self) -> DiscoveryFuture<'_> {
        Box::pin(async {})
    }
}
