use std::{net::Ipv6Addr, sync::Arc};

use librqbit::{
    DhtSessionConfig, ListenerMode, ListenerOptions, Session, SessionOptions,
    dht::DhtPersistenceConfig,
};

use crate::{Engine, EngineConfig, EngineStatus, Error};

const SCRATCH_DIR: &str = "scratch";
const DHT_CACHE_FILE: &str = "dht.json";

/// [`Engine`] backed by a `librqbit` session. The only place in Rustorr that
/// names librqbit types.
pub struct LibrqbitEngine {
    session: Arc<Session>,
    status: EngineStatus,
}

impl LibrqbitEngine {
    /// Starts a session and checks that the engine applied what was asked.
    ///
    /// Run this on a multi-thread tokio runtime. librqbit moves blocking
    /// storage work off the async threads with `block_in_place`; on a
    /// current-thread runtime it instead runs that work inline and stalls the
    /// whole runtime.
    pub async fn start(config: EngineConfig) -> Result<Self, Error> {
        tokio::fs::create_dir_all(&config.data_dir)
            .await
            .map_err(|source| Error::DataDir {
                path: config.data_dir.clone(),
                source,
            })?;

        let session =
            Session::new_with_opts(config.data_dir.join(SCRATCH_DIR), session_options(&config))
                .await
                .map_err(|error| Error::Start(error.into()))?;

        let status = EngineStatus {
            dht_enabled: session.get_dht().is_some(),
            listen_port: session.listen_addr().map(|addr| addr.port()),
        };
        if let Err(error) = verify(&config, &status) {
            session.stop().await;
            return Err(error);
        }
        Ok(Self { session, status })
    }

    /// Pauses torrents and stops the session's tasks.
    pub async fn shutdown(self) {
        self.session.stop().await;
    }
}

impl Engine for LibrqbitEngine {
    fn status(&self) -> &EngineStatus {
        &self.status
    }
}

/// The single place session options are built.
///
/// Persistence and fast-resume stay off: fast-resume was only shown to work
/// for an unchanged output directory (R3), and torrent state belongs to
/// `rustorr-state`. The DHT cache is different: it holds routing-table nodes
/// that are safe to lose, so it is kept, but at an explicit path inside
/// `data_dir` because the library default is the OS cache directory.
///
/// Trackers, persistence and fast-resume cannot be read back from a running
/// session, so they are covered by unit tests on this function instead of by
/// [`verify`].
fn session_options(config: &EngineConfig) -> SessionOptions {
    SessionOptions {
        dht: config.enable_dht.then(|| DhtSessionConfig {
            bootstrap_addrs: None,
            port: None,
            persistence: Some(DhtPersistenceConfig {
                dump_interval: None,
                config_filename: Some(config.data_dir.join(DHT_CACHE_FILE)),
            }),
        }),
        disable_trackers: !config.enable_trackers,
        fastresume: false,
        persistence: None,
        listen: config.listen_port.map(|port| ListenerOptions {
            mode: ListenerMode::TcpAndUtp,
            listen_addr: (Ipv6Addr::UNSPECIFIED, port).into(),
            ..ListenerOptions::default()
        }),
        ..SessionOptions::default()
    }
}

fn verify(config: &EngineConfig, status: &EngineStatus) -> Result<(), Error> {
    if status.dht_enabled != config.enable_dht {
        return Err(Error::SettingNotApplied {
            setting: "dht",
            expected: config.enable_dht.to_string(),
            actual: status.dht_enabled.to_string(),
        });
    }

    let listener_applied = match (config.listen_port, status.listen_port) {
        (None, None) => true,
        (Some(0), Some(bound)) => bound != 0,
        (Some(wanted), Some(bound)) => wanted == bound,
        _ => false,
    };
    if !listener_applied {
        return Err(Error::SettingNotApplied {
            setting: "listen port",
            expected: format!("{:?}", config.listen_port),
            actual: format!("{:?}", status.listen_port),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn config(data_dir: impl Into<PathBuf>) -> EngineConfig {
        EngineConfig {
            data_dir: data_dir.into(),
            listen_port: None,
            enable_dht: false,
            enable_trackers: false,
        }
    }

    #[test]
    fn engine_persistence_and_fastresume_are_always_off() {
        for enable in [false, true] {
            let options = session_options(&EngineConfig {
                enable_dht: enable,
                enable_trackers: enable,
                listen_port: enable.then_some(0),
                ..config("/data")
            });
            assert!(options.persistence.is_none());
            assert!(!options.fastresume);
        }
    }

    #[test]
    fn trackers_flag_is_applied_at_session_level() {
        let off = session_options(&config("/data"));
        let on = session_options(&EngineConfig {
            enable_trackers: true,
            ..config("/data")
        });
        assert!(off.disable_trackers);
        assert!(!on.disable_trackers);
    }

    #[test]
    fn dht_is_absent_when_disabled() {
        assert!(session_options(&config("/data")).dht.is_none());
    }

    #[test]
    fn dht_cache_is_pinned_inside_the_data_dir() {
        let options = session_options(&EngineConfig {
            enable_dht: true,
            ..config("/data")
        });
        let persistence = options.dht.unwrap().persistence.unwrap();
        assert_eq!(
            persistence.config_filename,
            Some(PathBuf::from("/data/dht.json"))
        );
    }

    #[test]
    fn listener_serves_tcp_and_utp_on_the_requested_port() {
        assert!(session_options(&config("/data")).listen.is_none());

        let listen = session_options(&EngineConfig {
            listen_port: Some(51413),
            ..config("/data")
        })
        .listen
        .unwrap();
        assert!(matches!(listen.mode, ListenerMode::TcpAndUtp));
        assert_eq!(listen.listen_addr.port(), 51413);
    }

    #[test]
    fn verify_accepts_matching_status() {
        let status = EngineStatus {
            dht_enabled: true,
            listen_port: Some(51413),
        };
        let wanted = EngineConfig {
            enable_dht: true,
            listen_port: Some(51413),
            ..config("/data")
        };
        assert!(verify(&wanted, &status).is_ok());
        // An ephemeral request is satisfied by any real port.
        let ephemeral = EngineConfig {
            listen_port: Some(0),
            ..wanted
        };
        assert!(verify(&ephemeral, &status).is_ok());
    }

    #[test]
    fn verify_reports_a_dht_that_did_not_start() {
        let status = EngineStatus {
            dht_enabled: false,
            listen_port: None,
        };
        let wanted = EngineConfig {
            enable_dht: true,
            ..config("/data")
        };
        assert!(matches!(
            verify(&wanted, &status),
            Err(Error::SettingNotApplied { setting: "dht", .. })
        ));
    }

    #[test]
    fn verify_reports_listener_mismatches() {
        let bound = |port| EngineStatus {
            dht_enabled: false,
            listen_port: port,
        };
        let listening_on = |port| EngineConfig {
            listen_port: port,
            ..config("/data")
        };
        let cases = [
            (listening_on(Some(51413)), bound(None)),
            (listening_on(None), bound(Some(51413))),
            (listening_on(Some(51413)), bound(Some(51414))),
            (listening_on(Some(0)), bound(Some(0))),
        ];
        for (wanted, status) in cases {
            assert!(
                matches!(
                    verify(&wanted, &status),
                    Err(Error::SettingNotApplied {
                        setting: "listen port",
                        ..
                    })
                ),
                "{wanted:?} vs {status:?}"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn starts_without_network_and_reports_what_it_applied() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().join("engine");

        let engine = LibrqbitEngine::start(config(&data_dir)).await.unwrap();

        assert_eq!(
            engine.status(),
            &EngineStatus {
                dht_enabled: false,
                listen_port: None
            }
        );
        assert!(data_dir.is_dir());
        engine.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn binds_a_listener_when_asked() {
        let dir = tempfile::tempdir().unwrap();

        let engine = LibrqbitEngine::start(EngineConfig {
            listen_port: Some(0),
            ..config(dir.path())
        })
        .await
        .unwrap();

        assert!(engine.status().listen_port.is_some_and(|port| port != 0));
        engine.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn starts_the_dht_when_asked() {
        let dir = tempfile::tempdir().unwrap();

        let engine = LibrqbitEngine::start(EngineConfig {
            enable_dht: true,
            ..config(dir.path())
        })
        .await
        .unwrap();

        assert!(engine.status().dht_enabled);
        engine.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn reports_an_unusable_data_dir() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("occupied");
        std::fs::write(&file, b"").unwrap();

        let error = LibrqbitEngine::start(config(file.join("engine")))
            .await
            .err()
            .unwrap();

        assert!(matches!(error, Error::DataDir { .. }), "{error}");
    }
}
