//! Commands for running Rustorr as a service: a health check for container
//! runtimes, and backup and restore of the state in the data directory.
//!
//! A backup is a `.tar.gz` of the database (a consistent snapshot, taken
//! while the server may run), the files an operator puts beside it and a
//! manifest. The cache and the engine's scratch files are left out: torrents
//! download again, and the catalog keeps their metainfo.

use std::{
    fs::{self, File},
    io::{Read, Write},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream},
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, bail, ensure};
use flate2::{Compression, read::GzDecoder, write::GzEncoder};
use serde_json::{Value, json};

const DATABASE: &str = "rustorr.db";
const MANIFEST: &str = "backup.json";
const FORMAT: u64 = 1;
/// Files beside the database that belong to the state: accounts, the extra
/// trackers list, the self-signed HTTPS pair and an operator's own pair as
/// `tools/deploy.sh cert` stores it.
const STATE_FILES: &[&str] = &[
    "accs.db",
    "trackers.txt",
    "server.pem",
    "server.key",
    "tls-cert.pem",
    "tls-key.pem",
];

/// Whether the server answers HTTP on `listen`: any response counts, since
/// `/echo` may sit behind authentication or a redirect to HTTPS.
pub fn health(listen: SocketAddr, timeout: Duration) -> anyhow::Result<u16> {
    // A server bound to every address is asked on loopback.
    let target = match listen.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => {
            SocketAddr::new(Ipv4Addr::LOCALHOST.into(), listen.port())
        }
        IpAddr::V6(ip) if ip.is_unspecified() => {
            SocketAddr::new(Ipv6Addr::LOCALHOST.into(), listen.port())
        }
        _ => listen,
    };
    let mut stream = TcpStream::connect_timeout(&target, timeout)
        .with_context(|| format!("cannot connect to {target}"))?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    write!(
        stream,
        "GET /echo HTTP/1.1\r\nHost: {target}\r\nConnection: close\r\n\r\n"
    )?;
    let mut head = [0; 64];
    let read = stream.read(&mut head)?;
    let line = String::from_utf8_lossy(&head[..read]);
    let status = line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .with_context(|| format!("not an HTTP answer from {target}"))?;
    ensure!(status < 500, "{target} answered {status}");
    Ok(status)
}

/// Writes a backup of `data_dir` to `file`, readable by its owner only.
pub fn backup(data_dir: &Path, file: &Path) -> anyhow::Result<Vec<String>> {
    let database = data_dir.join(DATABASE);
    ensure!(
        database.is_file(),
        "there is no database at {}",
        database.display()
    );
    let parent = file
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent).with_context(|| format!("cannot create {}", parent.display()))?;
    let staging = tempfile::tempdir_in(parent).context("cannot create a staging directory")?;

    let schema = rustorr_state::snapshot(&database, &staging.path().join(DATABASE))
        .with_context(|| format!("cannot copy {}", database.display()))?;
    let mut files = vec![DATABASE.to_owned()];
    for name in STATE_FILES {
        let path = data_dir.join(name);
        if path.is_file() {
            fs::copy(&path, staging.path().join(name))
                .with_context(|| format!("cannot copy {}", path.display()))?;
            files.push((*name).to_owned());
        }
    }
    let created = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let manifest = json!({
        "format": FORMAT,
        "rustorr": env!("CARGO_PKG_VERSION"),
        "schema": schema,
        "created": created,
        "files": files,
    });
    fs::write(
        staging.path().join(MANIFEST),
        serde_json::to_vec_pretty(&manifest)?,
    )?;

    // Written aside and renamed, so a failed backup never leaves half a file
    // under the requested name.
    let partial = tempfile::Builder::new()
        .prefix(".backup-")
        .tempfile_in(parent)
        .context("cannot create the backup file")?;
    {
        let mut archive =
            tar::Builder::new(GzEncoder::new(partial.as_file(), Compression::default()));
        archive.mode(tar::HeaderMode::Deterministic);
        for name in files.iter().map(String::as_str).chain([MANIFEST]) {
            archive.append_path_with_name(staging.path().join(name), name)?;
        }
        archive.into_inner()?.finish()?;
    }
    partial.as_file().sync_all()?;
    // tempfile creates it with mode 0600 on Unix: the accounts stay private.
    partial
        .persist(file)
        .with_context(|| format!("cannot write {}", file.display()))?;
    Ok(files)
}

/// Replaces the state in `data_dir` with the backup in `file`. The server
/// must be stopped. Without `force`, an existing database is left alone.
pub fn restore(data_dir: &Path, file: &Path, force: bool) -> anyhow::Result<Vec<String>> {
    fs::create_dir_all(data_dir)
        .with_context(|| format!("cannot create {}", data_dir.display()))?;
    let staging = tempfile::tempdir_in(data_dir).context("cannot create a staging directory")?;
    let input = File::open(file).with_context(|| format!("cannot open {}", file.display()))?;
    let mut archive = tar::Archive::new(GzDecoder::new(input));
    let allowed: Vec<&str> = [DATABASE, MANIFEST]
        .iter()
        .chain(STATE_FILES)
        .copied()
        .collect();
    for entry in archive.entries().context("not a Rustorr backup")? {
        let mut entry = entry.context("not a Rustorr backup")?;
        let path = entry.path()?.into_owned();
        // Plain names only: nothing may land outside the staging directory.
        let name = path.to_str().filter(|name| allowed.contains(name));
        let Some(name) = name else {
            bail!("unexpected entry {} in the backup", path.display());
        };
        ensure!(
            entry.header().entry_type().is_file(),
            "{name} in the backup is not a file"
        );
        entry.unpack(staging.path().join(name))?;
    }

    let manifest: Value = serde_json::from_slice(
        &fs::read(staging.path().join(MANIFEST)).context("the backup has no manifest")?,
    )
    .context("the backup's manifest is not JSON")?;
    ensure!(
        manifest["format"].as_u64() == Some(FORMAT),
        "unknown backup format {}",
        manifest["format"]
    );
    let restored = staging.path().join(DATABASE);
    ensure!(restored.is_file(), "the backup has no database");
    rustorr_state::inspect(&restored)
        .context("the backup's database cannot be used by this build")?;

    let database = data_dir.join(DATABASE);
    if database.exists() && !force {
        bail!(
            "{} already holds a database; stop the server and pass --force to replace it",
            data_dir.display()
        );
    }
    for name in [DATABASE, "rustorr.db-wal", "rustorr.db-shm"]
        .iter()
        .chain(STATE_FILES)
    {
        let path = data_dir.join(name);
        if path.exists() {
            fs::remove_file(&path).with_context(|| format!("cannot remove {}", path.display()))?;
        }
    }
    let mut files = Vec::new();
    for name in [DATABASE].iter().chain(STATE_FILES) {
        let source = staging.path().join(name);
        if source.is_file() {
            fs::rename(&source, data_dir.join(name))?;
            files.push((*name).to_owned());
        }
    }
    Ok(files)
}

/// Adds or updates an account in `<data-dir>/accs.db`, MatriX.145's JSON
/// map of user to password, keeping the file private to its owner.
pub fn set_password(data_dir: &Path, user: &str, password: &str) -> anyhow::Result<()> {
    ensure!(!user.is_empty(), "the user name is empty");
    ensure!(!password.is_empty(), "the password is empty");
    ensure!(
        !user.contains(':'),
        "a user name cannot contain `:` in HTTP Basic authentication"
    );
    let path = data_dir.join("accs.db");
    let mut accounts: serde_json::Map<String, Value> = match fs::read(&path) {
        Ok(bytes) if !bytes.is_empty() => serde_json::from_slice(&bytes)
            .with_context(|| format!("{} is not an accounts file", path.display()))?,
        Ok(_) => serde_json::Map::new(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => serde_json::Map::new(),
        Err(error) => return Err(error).with_context(|| format!("cannot read {}", path.display())),
    };
    accounts.insert(user.to_owned(), Value::String(password.to_owned()));
    fs::create_dir_all(data_dir)
        .with_context(|| format!("cannot create {}", data_dir.display()))?;
    let mut file = tempfile::Builder::new()
        .prefix(".accs-")
        .tempfile_in(data_dir)
        .context("cannot write the accounts file")?;
    file.write_all(&serde_json::to_vec(&accounts)?)?;
    file.as_file().sync_all()?;
    file.persist(&path)
        .with_context(|| format!("cannot write {}", path.display()))?;
    Ok(())
}

/// The first address the server listens on, for `health`.
pub fn first_listen(listen: &[SocketAddr]) -> SocketAddr {
    listen
        .first()
        .copied()
        .unwrap_or_else(|| SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 8090))
}

#[cfg(test)]
mod tests {
    use std::{io::BufRead, net::TcpListener, os::unix::fs::PermissionsExt};

    use rustorr_state::{CatalogEntry, State};

    use super::*;

    fn seed(dir: &Path, title: &str) {
        let state = State::open(dir.join(DATABASE)).unwrap();
        state
            .save_torrent(
                &CatalogEntry {
                    hash: "d272ca49e3f32a0a08c0c0599a0a9daa6bf5cb7d".parse().unwrap(),
                    title: title.into(),
                    poster: String::new(),
                    category: String::new(),
                    data: String::new(),
                    added_at: UNIX_EPOCH,
                    size: 1,
                },
                b"d4:infode",
            )
            .unwrap();
        fs::write(dir.join("accs.db"), br#"{"admin":"secret"}"#).unwrap();
    }

    fn title(dir: &Path) -> String {
        let state = State::open(dir.join(DATABASE)).unwrap();
        state
            .torrent("d272ca49e3f32a0a08c0c0599a0a9daa6bf5cb7d".parse().unwrap())
            .unwrap()
            .unwrap()
            .title
    }

    #[test]
    fn a_backup_restores_into_an_empty_directory() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        fs::create_dir(&data).unwrap();
        seed(&data, "Фильм");
        let file = dir.path().join("backups/state.tar.gz");

        let saved = backup(&data, &file).unwrap();
        assert_eq!(saved, ["rustorr.db", "accs.db"]);
        assert_eq!(
            fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );

        let fresh = dir.path().join("fresh");
        assert_eq!(
            restore(&fresh, &file, false).unwrap(),
            ["rustorr.db", "accs.db"]
        );
        assert_eq!(title(&fresh), "Фильм");
        assert_eq!(
            fs::read(fresh.join("accs.db")).unwrap(),
            br#"{"admin":"secret"}"#
        );
    }

    #[test]
    fn restore_keeps_existing_state_unless_forced() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        fs::create_dir(&data).unwrap();
        seed(&data, "Старый");
        let file = dir.path().join("state.tar.gz");
        backup(&data, &file).unwrap();
        seed(&data, "Новый");
        fs::write(data.join("trackers.txt"), "http://t/announce").unwrap();

        let refused = restore(&data, &file, false).unwrap_err().to_string();
        assert!(refused.contains("--force"), "{refused}");
        assert_eq!(title(&data), "Новый");

        restore(&data, &file, true).unwrap();
        assert_eq!(title(&data), "Старый");
        // The backup had no trackers file, so the restored state has none.
        assert!(!data.join("trackers.txt").exists());
    }

    #[test]
    fn entries_outside_the_known_files_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("evil.tar.gz");
        {
            let mut archive = tar::Builder::new(GzEncoder::new(
                File::create(&file).unwrap(),
                Compression::default(),
            ));
            let mut header = tar::Header::new_gnu();
            header.set_size(1);
            header.set_mode(0o644);
            header.set_cksum();
            archive
                .append_data(&mut header, "notes.txt", &b"x"[..])
                .unwrap();
            archive.into_inner().unwrap().finish().unwrap();
        }
        let error = restore(&dir.path().join("data"), &file, false)
            .unwrap_err()
            .to_string();
        assert!(error.contains("unexpected entry notes.txt"), "{error}");
    }

    #[test]
    fn passwords_are_added_and_changed_in_a_private_file() {
        let dir = tempfile::tempdir().unwrap();
        set_password(dir.path(), "admin", "one\"two").unwrap();
        set_password(dir.path(), "tv", "x").unwrap();
        set_password(dir.path(), "admin", "three").unwrap();
        let path = dir.path().join("accs.db");
        let accounts: serde_json::Map<String, Value> =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(accounts["admin"], "three");
        assert_eq!(accounts["tv"], "x");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(rustorr_http::Credentials::read(&path).is_ok());
        assert!(set_password(dir.path(), "a:b", "x").is_err());
    }

    #[test]
    fn health_accepts_any_http_answer_below_500() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for answer in [
                "HTTP/1.1 401 Unauthorized\r\n\r\n",
                "HTTP/1.1 503 Busy\r\n\r\n",
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = String::new();
                std::io::BufReader::new(&stream)
                    .read_line(&mut request)
                    .unwrap();
                assert_eq!(request, "GET /echo HTTP/1.1\r\n");
                stream.write_all(answer.as_bytes()).unwrap();
            }
        });
        assert_eq!(health(address, Duration::from_secs(2)).unwrap(), 401);
        assert!(health(address, Duration::from_secs(2)).is_err());
        server.join().unwrap();
        assert!(health(address, Duration::from_millis(200)).is_err());
    }
}
