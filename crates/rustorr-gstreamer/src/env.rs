//! Where GStreamer lives on Linux, as `pipeline_gst.go` and `probe.go` look
//! for it, and `gst-discoverer-1.0` run with the environment the reference
//! gives it.

use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use serde::Serialize;
use tokio::process::Command;

use crate::{Config, Error};

const DEFAULT_ROOTS: [&str; 4] = ["/usr", "/usr/local", "/opt/gstreamer", "/opt/gstreamer/1.0"];
const DISCOVERER: &str = "gst-discoverer-1.0";
/// `gstProbeTimeout`.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(30);

/// `componentStatus`, one entry of `/gst/echo`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ComponentStatus {
    pub found: bool,
    pub available: bool,
    pub works: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub version: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub error: String,
}

fn go_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => other,
    }
}

/// `gstLibraryDirCandidates`. Go joins `runtime.GOARCH`, so the first
/// multiarch guess is `amd64-linux-gnu`/`arm64-linux-gnu`, which never
/// exists; the two spelled-out directories follow.
fn library_dirs(roots: &[PathBuf]) -> Vec<PathBuf> {
    roots
        .iter()
        .flat_map(|root| {
            [
                root.join("lib"),
                root.join("lib64"),
                root.join("lib").join(format!("{}-linux-gnu", go_arch())),
                root.join("lib").join("x86_64-linux-gnu"),
                root.join("lib").join("aarch64-linux-gnu"),
            ]
        })
        .collect()
}

fn plugin_dirs(roots: &[PathBuf]) -> Vec<PathBuf> {
    library_dirs(roots)
        .into_iter()
        .map(|dir| dir.join("gstreamer-1.0"))
        .collect()
}

fn scanner_candidates(roots: &[PathBuf]) -> Vec<PathBuf> {
    roots
        .iter()
        .flat_map(|root| {
            [
                root.join("libexec/gstreamer-1.0/gst-plugin-scanner"),
                root.join("lib/gstreamer-1.0/gst-plugin-scanner"),
                root.join("lib64/gstreamer-1.0/gst-plugin-scanner"),
            ]
        })
        .collect()
}

fn is_file(path: &Path) -> bool {
    path.metadata().is_ok_and(|metadata| !metadata.is_dir())
}

fn first_existing(candidates: Vec<PathBuf>) -> Option<PathBuf> {
    candidates.into_iter().find(|candidate| candidate.exists())
}

/// `gstRuntimeRoots`: `GSTPath` and the default prefixes that hold
/// `libgstreamer-1.0.so.0`.
pub fn runtime_roots(config: &Config) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    let candidates = std::iter::once(config.gst_path.as_str()).chain(DEFAULT_ROOTS);
    for root in candidates
        .filter(|root| !root.is_empty())
        .map(PathBuf::from)
    {
        let has_library = library_dirs(std::slice::from_ref(&root))
            .iter()
            .any(|dir| is_file(&dir.join("libgstreamer-1.0.so.0")));
        if has_library && !roots.contains(&root) {
            roots.push(root);
        }
    }
    roots
}

/// `gstreamerLibraryFound`.
pub fn library_found(config: &Config) -> bool {
    library_dirs(&runtime_roots(config))
        .iter()
        .any(|dir| is_file(&dir.join("libgstreamer-1.0.so.0")))
}

/// `gstDiscovererPathRoot`: under a runtime root first, then `PATH`.
fn discoverer(config: &Config) -> Result<(PathBuf, Option<PathBuf>), Error> {
    for root in runtime_roots(config) {
        let path = root.join("bin").join(DISCOVERER);
        if is_file(&path) {
            return Ok((path, Some(root)));
        }
    }
    let in_path = std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(DISCOVERER))
            .find(|candidate| is_executable(candidate))
    });
    in_path
        .map(|path| (path, None))
        .ok_or_else(|| Error::Other(format!("{DISCOVERER} not found")))
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

/// `gstDiscovererEnv`: C.UTF-8, no colours, and the selected root's
/// directories in front of the search paths.
fn discoverer_command(path: &Path, root: Option<&Path>) -> Command {
    let mut command = Command::new(path);
    command
        .env("LANG", "C.UTF-8")
        .env("LC_ALL", "C.UTF-8")
        .env("LANGUAGE", "en")
        .env("GST_DEBUG_NO_COLOR", "1");
    let Some(root) = root else {
        return command;
    };
    let roots = [root.to_path_buf()];
    let prepend = |key: &str, dirs: Vec<PathBuf>| {
        let mut parts: Vec<PathBuf> = Vec::new();
        for dir in dirs.into_iter().filter(|dir| dir.is_dir()) {
            if !parts.contains(&dir) {
                parts.push(dir);
            }
        }
        if parts.is_empty() {
            return None;
        }
        if let Some(current) = std::env::var_os(key) {
            for part in std::env::split_paths(&current) {
                if !part.as_os_str().is_empty() && !parts.contains(&part) {
                    parts.push(part);
                }
            }
        }
        std::env::join_paths(parts).ok()
    };
    if let Some(value) = prepend("PATH", vec![root.join("bin")]) {
        command.env("PATH", value);
    }
    if let Some(value) = prepend("LD_LIBRARY_PATH", library_dirs(&roots)) {
        command.env("LD_LIBRARY_PATH", value);
    }
    if let Some(plugins) = first_existing(plugin_dirs(&roots)) {
        command
            .env("GST_PLUGIN_PATH", &plugins)
            .env("GST_PLUGIN_SYSTEM_PATH_1_0", &plugins);
    }
    if let Some(scanner) = first_existing(scanner_candidates(&roots)) {
        command.env("GST_PLUGIN_SCANNER", scanner);
    }
    command
}

/// `runGSTDiscoverer`: combined output, and the error Go's `exec` reports.
pub async fn discover(url: &str, config: &Config) -> (String, Option<Error>) {
    let (path, root) = match discoverer(config) {
        Ok(found) => found,
        Err(error) => return (String::new(), Some(error)),
    };
    let mut command = discoverer_command(&path, root.as_deref());
    command
        .arg("-v")
        .arg("-t")
        .arg(PROBE_TIMEOUT.as_secs().to_string())
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let run = async {
        let output = command.output().await?;
        Ok::<_, std::io::Error>(output)
    };
    match tokio::time::timeout(PROBE_TIMEOUT + Duration::from_secs(3), run).await {
        Err(_) => (String::new(), Some(Error::DeadlineExceeded)),
        Ok(Err(error)) => (String::new(), Some(Error::Other(error.to_string()))),
        Ok(Ok(output)) => {
            // CombinedOutput interleaves both streams; discoverer writes its
            // report to stdout and warnings to stderr.
            let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
            text.push_str(&String::from_utf8_lossy(&output.stderr));
            let error = (!output.status.success()).then(|| Error::Other(exit_text(&output.status)));
            (text, error)
        }
    }
}

/// `exec.ExitError.Error()`.
fn exit_text(status: &std::process::ExitStatus) -> String {
    use std::os::unix::process::ExitStatusExt;
    match (status.code(), status.signal()) {
        (Some(code), _) => format!("exit status {code}"),
        (None, Some(signal)) => format!("signal: {}", signal_name(signal)),
        _ => "exit status -1".into(),
    }
}

fn signal_name(signal: i32) -> String {
    match signal {
        9 => "killed".into(),
        15 => "terminated".into(),
        6 => "aborted".into(),
        11 => "segmentation fault".into(),
        other => format!("signal {other}"),
    }
}

/// `checkGSTDiscoverer`: found, a file, and `-h` succeeds within three
/// seconds.
pub async fn discoverer_status(config: &Config) -> ComponentStatus {
    let mut status = ComponentStatus::default();
    let Ok((path, root)) = discoverer(config) else {
        return status;
    };
    status.found = true;
    if !is_file(&path) {
        return status;
    }
    status.available = true;
    let mut command = discoverer_command(&path, root.as_deref());
    command
        .arg("-h")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    match tokio::time::timeout(Duration::from_secs(3), command.status()).await {
        Ok(Ok(exit)) if exit.success() => status.works = true,
        Ok(Ok(exit)) => status.error = exit_text(&exit),
        Ok(Err(error)) => status.error = error.to_string(),
        Err(_) => status.error = "signal: killed".into(),
    }
    status
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn library_directories_follow_the_reference_order() {
        let dirs = library_dirs(&[PathBuf::from("/usr")]);
        assert_eq!(dirs[0], PathBuf::from("/usr/lib"));
        assert_eq!(dirs[1], PathBuf::from("/usr/lib64"));
        assert!(
            dirs[2]
                .to_string_lossy()
                .ends_with(&format!("{}-linux-gnu", go_arch()))
        );
        assert_eq!(dirs[3], PathBuf::from("/usr/lib/x86_64-linux-gnu"));
        assert_eq!(dirs[4], PathBuf::from("/usr/lib/aarch64-linux-gnu"));
    }

    #[test]
    fn a_missing_discoverer_is_reported_like_go() {
        let status = ComponentStatus::default();
        assert_eq!(
            serde_json::to_string(&status).unwrap(),
            r#"{"found":false,"available":false,"works":false}"#
        );
    }
}
