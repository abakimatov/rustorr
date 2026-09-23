//! Bonjour and the DLNA media server, started and stopped as MatriX.145
//! does: from the settings at start-up, on every `/settings` change, and —
//! for DLNA — after each catalog change.

use std::{net::IpAddr, sync::Arc, time::Duration, time::SystemTime};

use rustorr_discovery::{Bonjour, BonjourConfig, Ssdp, SsdpConfig, identity, interfaces};
use rustorr_http::{Discovery, DiscoveryChange, DiscoveryFuture, DlnaDevice};
use rustorr_lifecycle::{ClientCore, Settings, SettingsCommand};
use tokio::{net::TcpListener, sync::Mutex, sync::oneshot, task::JoinHandle};
use tracing::{info, warn};

/// dms's SSDP notify interval.
const NOTIFY_INTERVAL: Duration = Duration::from_secs(30);
/// The first port the DLNA HTTP server tries.
const DLNA_FIRST_PORT: u16 = 9080;

pub struct DiscoveryService {
    core: Arc<dyn ClientCore>,
    bind: IpAddr,
    web_port: u16,
    version: String,
    client: reqwest::Client,
    started: SystemTime,
    bonjour: Bonjour,
    ssdp: Ssdp,
    dlna: Mutex<Option<DlnaServer>>,
}

struct DlnaServer {
    stop: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

/// How long a stopping DLNA server may finish requests in flight.
const DLNA_GRACE: Duration = Duration::from_secs(1);

impl DiscoveryService {
    pub fn new(
        core: Arc<dyn ClientCore>,
        bind: IpAddr,
        web_port: u16,
        version: String,
        client: reqwest::Client,
        started: SystemTime,
    ) -> Self {
        Self {
            core,
            bind,
            web_port,
            version,
            client,
            started,
            bonjour: Bonjour::new(),
            ssdp: Ssdp::new(),
            dlna: Mutex::new(None),
        }
    }

    /// Start-up: each service as the stored settings enable it.
    pub async fn start(&self, settings: &Settings) {
        if settings.enable_dlna {
            self.start_dlna(&settings.friendly_name).await;
        }
        if settings.enable_bonjour {
            self.start_bonjour(&settings.friendly_name).await;
        }
    }

    /// Shutdown: goodbye packets for both.
    pub async fn stop(&self) {
        self.stop_dlna().await;
        self.bonjour.stop().await;
    }

    async fn start_bonjour(&self, friendly_name: &str) {
        self.bonjour
            .start(BonjourConfig {
                friendly_name: friendly_name.into(),
                port: self.web_port,
                version: self.version.clone(),
            })
            .await;
    }

    /// `dlna.Start`: the first free port from 9080, then SSDP announcing it.
    async fn start_dlna(&self, configured_name: &str) {
        let mut running = self.dlna.lock().await;
        let friendly_name = identity::dlna_friendly_name(
            configured_name,
            identity::user_full_name().as_deref(),
            identity::hostname().as_deref(),
            &interfaces::list(),
        );
        let udn = identity::device_uuid(&friendly_name);
        let mut port = DLNA_FIRST_PORT;
        let listener = loop {
            match TcpListener::bind((self.bind, port)).await {
                Ok(listener) => break listener,
                Err(_) if port < u16::MAX => port += 1,
                Err(error) => {
                    warn!(%error, "dlna listen failed");
                    return;
                }
            }
        };
        info!(port, %friendly_name, "DLNA server started");
        let device = DlnaDevice {
            core: Arc::clone(&self.core),
            friendly_name: friendly_name.clone(),
            udn: udn.clone(),
            web_port: self.web_port,
            client: self.client.clone(),
            started: self.started,
        };
        let (stop, stopped) = oneshot::channel();
        let task = tokio::spawn(async move {
            let shutdown = async move {
                let _ = stopped.await;
            };
            if let Err(error) = rustorr_http::serve_dlna(listener, device, shutdown).await {
                warn!(%error, "DLNA server failed");
            }
        });
        self.ssdp
            .start(SsdpConfig {
                uuid: udn,
                http_port: port,
                notify_interval: NOTIFY_INTERVAL,
            })
            .await;
        *running = Some(DlnaServer { stop, task });
    }

    /// `dlna.Stop`: the HTTP server closes, then SSDP says goodbye.
    async fn stop_dlna(&self) {
        let Some(mut server) = self.dlna.lock().await.take() else {
            return;
        };
        let _ = server.stop.send(());
        if tokio::time::timeout(DLNA_GRACE, &mut server.task)
            .await
            .is_err()
        {
            server.task.abort();
            let _ = server.task.await;
        }
        self.ssdp.stop().await;
    }
}

impl Discovery for DiscoveryService {
    fn settings_changed(&self, change: DiscoveryChange) -> DiscoveryFuture<'_> {
        Box::pin(async move {
            match change {
                DiscoveryChange::Set {
                    dlna,
                    bonjour,
                    settings,
                } => {
                    self.stop_dlna().await;
                    if dlna {
                        self.start_dlna(&settings.friendly_name).await;
                    }
                    self.bonjour.stop().await;
                    if bonjour {
                        self.start_bonjour(&settings.friendly_name).await;
                    }
                }
                DiscoveryChange::Defaults => {
                    self.stop_dlna().await;
                    self.bonjour.stop().await;
                }
            }
        })
    }

    fn catalog_changed(&self) -> DiscoveryFuture<'_> {
        Box::pin(async move {
            // The HTTP layer calls this only while `EnableDLNA` is stored;
            // the server restarts with the settings in effect.
            let Ok(settings) = self.core.settings(SettingsCommand::Get).await else {
                return;
            };
            self.stop_dlna().await;
            self.start_dlna(&settings.friendly_name).await;
        })
    }
}
