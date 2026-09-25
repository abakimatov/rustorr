//! MatriX.145's `--ssl`: HTTPS on a second port, with the certificate and key
//! given by flags or remembered in the settings, else a self-signed pair made
//! in the data directory. Flags are stored in the settings, as the reference
//! does, so a later start without them keeps using the same port and files.

use std::{
    fs,
    io::Write,
    net::IpAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime},
};

use anyhow::{Context, bail, ensure};
use rustls::{ServerConfig, crypto::aws_lc_rs, sign::CertifiedKey};
use rustls_pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use rustorr_lifecycle::Settings;
use rustorr_state::State;
use tracing::{info, warn};

/// `--ssl-port` when neither the flag nor the settings give one.
pub const DEFAULT_PORT: u16 = 8091;
const SELF_SIGNED_DAYS: u64 = 365;

pub struct Options<'a> {
    pub enabled: bool,
    pub port: Option<u16>,
    pub cert: Option<&'a Path>,
    pub key: Option<&'a Path>,
    pub force_https: bool,
    pub read_only: bool,
}

pub struct Https {
    pub port: u16,
    pub config: Arc<ServerConfig>,
}

/// Settles the HTTPS port and certificate, storing what changed, and loads
/// the certificate. `None` without `--ssl`.
pub fn prepare(
    options: &Options<'_>,
    data_dir: &Path,
    state: &State,
) -> anyhow::Result<Option<Https>> {
    if !options.enabled {
        ensure!(!options.force_https, "--force-https requires --ssl");
        return Ok(None);
    }
    let stored = state.settings().context("cannot read the settings")?;
    let mut settings: Settings = stored
        .as_deref()
        .and_then(|document| serde_json::from_str(document).ok())
        .unwrap_or_default();
    let before = settings.clone();

    let port = match options.port {
        Some(port) => {
            settings.ssl_port = i32::from(port);
            port
        }
        None => u16::try_from(settings.ssl_port)
            .ok()
            .filter(|port| *port != 0)
            .unwrap_or(DEFAULT_PORT),
    };
    if let (Some(cert), Some(key)) = (options.cert, options.key) {
        settings.ssl_cert = absolute(cert).display().to_string();
        settings.ssl_key = absolute(key).display().to_string();
    } else if options.cert.is_some() || options.key.is_some() {
        bail!("--ssl-cert and --ssl-key go together");
    }
    if settings.ssl_cert.is_empty() || settings.ssl_key.is_empty() {
        let (cert, key) = self_signed(data_dir)?;
        settings.ssl_cert = cert.display().to_string();
        settings.ssl_key = key.display().to_string();
    }
    let config = match load(Path::new(&settings.ssl_cert), Path::new(&settings.ssl_key)) {
        Ok(config) => config,
        Err(error) => {
            // As the reference: an unusable pair is replaced, not fatal.
            warn!(
                cert = %settings.ssl_cert,
                error = format!("{error:#}"),
                "the HTTPS certificate cannot be used; using a new self-signed one"
            );
            let (cert, key) = self_signed(data_dir)?;
            settings.ssl_cert = cert.display().to_string();
            settings.ssl_key = key.display().to_string();
            load(&cert, &key)?
        }
    };
    if settings != before {
        if options.read_only {
            info!("read-only mode: the HTTPS port and certificate paths are not stored");
        } else {
            state
                .set_settings(&serde_json::to_string(&settings)?)
                .context("cannot store the HTTPS settings")?;
        }
    }
    info!(port, cert = %settings.ssl_cert, "HTTPS enabled");
    Ok(Some(Https { port, config }))
}

fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_owned())
}

/// A certificate chain and its key as a server configuration, refusing an
/// expired certificate and a key that does not belong to it.
pub fn load(cert: &Path, key: &Path) -> anyhow::Result<Arc<ServerConfig>> {
    let chain = CertificateDer::pem_file_iter(cert)
        .with_context(|| format!("cannot read {}", cert.display()))?
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| format!("{} is not a PEM certificate", cert.display()))?;
    ensure!(!chain.is_empty(), "{} holds no certificate", cert.display());
    for certificate in &chain {
        let (_, parsed) = x509_parser::parse_x509_certificate(certificate).map_err(|error| {
            anyhow::anyhow!("{} is not an X.509 certificate: {error}", cert.display())
        })?;
        let not_after = parsed.validity().not_after.timestamp();
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs());
        ensure!(
            i64::try_from(now).unwrap_or(i64::MAX) < not_after,
            "the certificate in {} has expired",
            cert.display()
        );
    }
    let private = PrivateKeyDer::from_pem_file(key)
        .with_context(|| format!("cannot read the key {}", key.display()))?;
    let provider = Arc::new(aws_lc_rs::default_provider());
    let signing = provider
        .key_provider
        .load_private_key(private.clone_key())
        .context("unsupported private key")?;
    CertifiedKey::new(chain.clone(), signing)
        .keys_match()
        .context("the key does not belong to the certificate")?;
    let mut config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(chain, private)?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

/// `sslcerts.MakeCertKeyFiles`: an ECDSA P-384 certificate for a year, for
/// `localhost` and this machine's addresses, written to `server.pem` and
/// `server.key` in the data directory.
pub fn self_signed(data_dir: &Path) -> anyhow::Result<(PathBuf, PathBuf)> {
    let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P384_SHA384)?;
    let mut params = rcgen::CertificateParams::new(vec!["localhost".to_owned()])?;
    let mut addresses: Vec<IpAddr> = rustorr_discovery::interfaces::list()
        .into_iter()
        .filter(|interface| interface.up)
        .flat_map(|interface| interface.addresses.into_iter().map(|address| address.ip))
        .collect();
    addresses.sort();
    addresses.dedup();
    params
        .subject_alt_names
        .extend(addresses.into_iter().map(rcgen::SanType::IpAddress));
    // rcgen's default subject names it; the reference gives only an
    // organisation.
    params.distinguished_name = rcgen::DistinguishedName::new();
    params
        .distinguished_name
        .push(rcgen::DnType::OrganizationName, "Rustorr");
    let now = time_from(SystemTime::now());
    params.not_before = now;
    params.not_after = now + Duration::from_secs(SELF_SIGNED_DAYS * 24 * 3600);
    params.is_ca = rcgen::IsCa::ExplicitNoCa;
    params.key_usages = vec![
        rcgen::KeyUsagePurpose::DigitalSignature,
        rcgen::KeyUsagePurpose::KeyEncipherment,
    ];
    params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
    let certificate = params.self_signed(&key)?;

    fs::create_dir_all(data_dir)
        .with_context(|| format!("cannot create {}", data_dir.display()))?;
    let cert_path = absolute(&data_dir.join("server.pem"));
    let key_path = absolute(&data_dir.join("server.key"));
    fs::write(&cert_path, certificate.pem())
        .with_context(|| format!("cannot write {}", cert_path.display()))?;
    write_private(&key_path, key.serialize_pem().as_bytes())?;
    info!(cert = %cert_path.display(), "self-signed HTTPS certificate generated");
    Ok((cert_path, key_path))
}

fn write_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let _ = fs::remove_file(path);
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("cannot write {}", path.display()))?;
    file.write_all(bytes)?;
    Ok(())
}

fn time_from(moment: SystemTime) -> time::OffsetDateTime {
    time::OffsetDateTime::from(moment)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn options(enabled: bool) -> Options<'static> {
        Options {
            enabled,
            port: None,
            cert: None,
            key: None,
            force_https: false,
            read_only: false,
        }
    }

    fn stored(state: &State) -> Settings {
        serde_json::from_str(&state.settings().unwrap().unwrap()).unwrap()
    }

    #[test]
    fn without_ssl_nothing_happens_and_force_https_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let state = State::open_in_memory().unwrap();
        assert!(
            prepare(&options(false), dir.path(), &state)
                .unwrap()
                .is_none()
        );
        let error = prepare(
            &Options {
                force_https: true,
                ..options(false)
            },
            dir.path(),
            &state,
        )
        .err()
        .unwrap();
        assert_eq!(error.to_string(), "--force-https requires --ssl");
    }

    #[test]
    fn a_self_signed_pair_is_made_once_and_remembered() {
        let dir = tempfile::tempdir().unwrap();
        let state = State::open_in_memory().unwrap();
        let https = prepare(&options(true), dir.path(), &state)
            .unwrap()
            .unwrap();
        assert_eq!(https.port, DEFAULT_PORT);
        let settings = stored(&state);
        assert!(settings.ssl_cert.ends_with("server.pem"));
        assert_eq!(
            fs::metadata(&settings.ssl_key)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let first = fs::read(&settings.ssl_cert).unwrap();

        // A second start keeps the files; a port flag is stored.
        let https = prepare(
            &Options {
                port: Some(9443),
                ..options(true)
            },
            dir.path(),
            &state,
        )
        .unwrap()
        .unwrap();
        assert_eq!(https.port, 9443);
        assert_eq!(fs::read(&settings.ssl_cert).unwrap(), first);
        assert_eq!(stored(&state).ssl_port, 9443);
        assert_eq!(
            prepare(&options(true), dir.path(), &state)
                .unwrap()
                .unwrap()
                .port,
            9443
        );
    }

    #[test]
    fn a_broken_pair_is_replaced_by_a_self_signed_one() {
        let dir = tempfile::tempdir().unwrap();
        let state = State::open_in_memory().unwrap();
        let (cert, _) = self_signed(&dir.path().join("a")).unwrap();
        let (_, other_key) = self_signed(&dir.path().join("b")).unwrap();
        assert!(
            load(&cert, &other_key)
                .unwrap_err()
                .to_string()
                .contains("does not belong")
        );
        let https = prepare(
            &Options {
                cert: Some(&cert),
                key: Some(&other_key),
                ..options(true)
            },
            dir.path(),
            &state,
        );
        assert!(https.unwrap().is_some());
        assert!(
            stored(&state)
                .ssl_cert
                .ends_with(&format!("{}server.pem", std::path::MAIN_SEPARATOR))
        );
        assert_eq!(
            Path::new(&stored(&state).ssl_cert).parent().unwrap(),
            absolute(dir.path())
        );
    }
}
