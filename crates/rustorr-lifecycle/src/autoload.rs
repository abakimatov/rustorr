//! MatriX.145's `--torrentsdir`: every `.torrent` file that appears in a
//! directory is saved to the catalog, unloaded, and deleted.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime},
};

use tracing::{info, warn};

use crate::{AddTorrent, TorrentCoordinator};

/// What a file looked like at one poll; equal twice in a row means the
/// writer has finished.
type Signature = (u64, Option<SystemTime>);

impl TorrentCoordinator {
    /// Polls `dir` every `interval`. The reference reacts to create and write
    /// events only, so files already present at start are left alone until
    /// they change. A file is taken once it is unchanged across two polls.
    pub async fn watch_torrents_dir(self: Arc<Self>, dir: PathBuf, interval: Duration) {
        let mut seen: HashMap<PathBuf, Signature> = torrent_files(&dir).into_iter().collect();
        let mut pending: HashMap<PathBuf, Signature> = HashMap::new();
        loop {
            tokio::time::sleep(interval).await;
            let current = torrent_files(&dir);
            pending.retain(|path, _| current.iter().any(|(present, _)| present == path));
            for (path, signature) in current {
                if seen.get(&path) == Some(&signature) {
                    continue;
                }
                if pending.get(&path) == Some(&signature) {
                    pending.remove(&path);
                    seen.insert(path.clone(), signature);
                    self.autoload(&path).await;
                } else {
                    pending.insert(path, signature);
                }
            }
        }
    }

    async fn autoload(&self, path: &Path) {
        let bytes = match tokio::fs::read(path).await {
            Ok(bytes) => bytes,
            Err(error) => {
                warn!(path = %path.display(), %error, "cannot read torrent file");
                return;
            }
        };
        let request = AddTorrent {
            save_to_db: true,
            ..AddTorrent::default()
        };
        let hash = match self.add_metainfo(bytes, request).await {
            Ok(view) => view.hash(),
            Err(error) => {
                warn!(path = %path.display(), %error, "cannot add torrent file");
                return;
            }
        };
        if let Some(hash) = hash
            && let Err(error) = self.drop_live(hash).await
        {
            warn!(%hash, %error, "cannot unload an autoloaded torrent");
        }
        match tokio::fs::remove_file(path).await {
            Ok(()) => info!(path = %path.display(), ?hash, "torrent file autoloaded"),
            Err(error) => warn!(path = %path.display(), %error, "cannot remove torrent file"),
        }
    }
}

fn torrent_files(dir: &Path) -> Vec<(PathBuf, Signature)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("torrent"))
        })
        .filter_map(|entry| {
            let metadata = entry.metadata().ok()?;
            metadata
                .is_file()
                .then(|| (entry.path(), (metadata.len(), metadata.modified().ok())))
        })
        .collect()
}
