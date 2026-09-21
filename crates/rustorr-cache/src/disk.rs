use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io,
    os::unix::fs::FileExt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock},
};

use rustorr_domain::{FileIndex, InfoHash};

use crate::{Error, PieceStore, TorrentLayout, extents::Extents};

/// SSD-backed store: one sparse file per torrent file under
/// `<root>/<info hash>/<file index>`, using positioned IO (Unix only).
///
/// Written ranges are tracked in memory only. After a restart the files left
/// on disk are unusable and `open` clears them; keeping the cache across
/// restarts is R5 work.
pub struct DiskStore {
    root: PathBuf,
    torrents: RwLock<HashMap<InfoHash, Arc<Mutex<Torrent>>>>,
}

struct Torrent {
    layout: TorrentLayout,
    dir: PathBuf,
    extents: Vec<Extents>,
}

impl Torrent {
    fn file_path(&self, file: FileIndex) -> PathBuf {
        self.dir.join(file.zero_based().to_string())
    }
}

fn io_error(context: &'static str) -> impl FnOnce(io::Error) -> Error {
    move |source| Error::Io { context, source }
}

fn lock(torrent: &Mutex<Torrent>) -> MutexGuard<'_, Torrent> {
    torrent.lock().unwrap_or_else(PoisonError::into_inner)
}

impl DiskStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            torrents: RwLock::default(),
        }
    }

    fn torrent(&self, torrent: InfoHash) -> Result<Arc<Mutex<Torrent>>, Error> {
        self.torrents
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&torrent)
            .cloned()
            .ok_or(Error::UnknownTorrent(torrent))
    }
}

fn remove_dir(dir: &Path) -> Result<(), Error> {
    match fs::remove_dir_all(dir) {
        Err(source) if source.kind() != io::ErrorKind::NotFound => Err(Error::Io {
            context: "remove cache directory",
            source,
        }),
        _ => Ok(()),
    }
}

impl PieceStore for DiskStore {
    fn open(&self, torrent: InfoHash, layout: &TorrentLayout) -> Result<(), Error> {
        let dir = self.root.join(torrent.to_string());
        remove_dir(&dir)?;
        fs::create_dir_all(&dir).map_err(io_error("create cache directory"))?;

        let created = Arc::new(Mutex::new(Torrent {
            layout: layout.clone(),
            dir,
            extents: (0..layout.file_count())
                .map(|_| Extents::default())
                .collect(),
        }));
        self.torrents
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(torrent, created);
        Ok(())
    }

    fn write(
        &self,
        torrent: InfoHash,
        file: FileIndex,
        offset: u64,
        data: &[u8],
    ) -> Result<u64, Error> {
        let handle = self.torrent(torrent)?;
        let (path, range) = {
            let guard = lock(&handle);
            let range = guard
                .layout
                .check_range(torrent, file, offset, data.len())?;
            (guard.file_path(file), range)
        };
        if data.is_empty() {
            return Ok(0);
        }

        // Positioned writes need no lock; a range counts as stored only once
        // it is on the file. Earlier ranges of the file must survive, so no
        // truncation.
        let target = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)
            .map_err(io_error("open cache file for writing"))?;
        target
            .write_all_at(data, offset)
            .map_err(io_error("write cache file"))?;

        let mut guard = lock(&handle);
        Ok(guard.extents[file.zero_based() as usize].insert(range))
    }

    fn read(
        &self,
        torrent: InfoHash,
        file: FileIndex,
        offset: u64,
        buf: &mut [u8],
    ) -> Result<(), Error> {
        let handle = self.torrent(torrent)?;
        let path = {
            let guard = lock(&handle);
            let range = guard.layout.check_range(torrent, file, offset, buf.len())?;
            if !guard.extents[file.zero_based() as usize].covers(range) {
                return Err(Error::Missing {
                    torrent,
                    file,
                    range,
                });
            }
            guard.file_path(file)
        };
        if buf.is_empty() {
            return Ok(());
        }

        File::open(path)
            .map_err(io_error("open cache file for reading"))?
            .read_exact_at(buf, offset)
            .map_err(io_error("read cache file"))
    }

    fn remove(&self, torrent: InfoHash) -> Result<(), Error> {
        let removed = self
            .torrents
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&torrent);
        match removed {
            Some(handle) => remove_dir(&lock(&handle).dir),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{file, layout, torrent};

    fn store() -> (DiskStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        (DiskStore::new(dir.path().join("cache")), dir)
    }

    crate::testing::store_contract_tests!(store());

    #[test]
    fn open_clears_files_left_by_a_previous_run() {
        let (store, dir) = store();
        let leftover = dir.path().join("cache").join(torrent(1).to_string());
        fs::create_dir_all(&leftover).unwrap();
        fs::write(leftover.join("0"), b"stale").unwrap();

        store.open(torrent(1), &layout()).unwrap();

        assert!(!leftover.join("0").exists());
        assert!(leftover.is_dir());
    }

    #[test]
    fn remove_deletes_the_torrent_directory_and_only_that() {
        let (store, dir) = store();
        store.open(torrent(1), &layout()).unwrap();
        store.open(torrent(2), &layout()).unwrap();
        store.write(torrent(1), file(0), 0, b"one").unwrap();
        store.write(torrent(2), file(0), 0, b"two").unwrap();

        store.remove(torrent(1)).unwrap();

        let cache = dir.path().join("cache");
        assert!(!cache.join(torrent(1).to_string()).exists());
        assert!(cache.join(torrent(2).to_string()).join("0").is_file());
    }

    #[test]
    fn a_file_deleted_behind_the_stores_back_is_an_error_not_zeros() {
        let (store, dir) = store();
        store.open(torrent(1), &layout()).unwrap();
        store.write(torrent(1), file(0), 0, &[7; 64]).unwrap();
        fs::remove_file(
            dir.path()
                .join("cache")
                .join(torrent(1).to_string())
                .join("0"),
        )
        .unwrap();

        let mut buf = [0; 64];
        let error = store.read(torrent(1), file(0), 0, &mut buf).unwrap_err();

        assert!(matches!(error, Error::Io { .. }), "{error}");
    }

    #[test]
    fn files_are_not_pre_sized_to_the_torrents_length() {
        let (store, dir) = store();
        store.open(torrent(1), &layout()).unwrap();
        store
            .write(torrent(1), file(0), 90_000, &[1; 1000])
            .unwrap();

        let path = dir
            .path()
            .join("cache")
            .join(torrent(1).to_string())
            .join("0");
        assert_eq!(fs::metadata(path).unwrap().len(), 91_000);
    }
}
