use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io,
    io::Write,
    os::unix::fs::FileExt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock},
};

use rustorr_domain::{FileIndex, InfoHash, PieceIndex};

use crate::{Error, PieceStore, Recovered, TorrentLayout, extents::Extents};

const MANIFEST: &str = ".rustorr-cache-v1";

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
    completed: Vec<bool>,
}

impl Torrent {
    fn file_path(&self, file: FileIndex) -> PathBuf {
        self.dir.join(file.zero_based().to_string())
    }

    fn manifest_path(&self) -> PathBuf {
        self.dir.join(MANIFEST)
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

fn empty_torrent(dir: PathBuf, layout: TorrentLayout) -> Torrent {
    Torrent {
        completed: vec![false; layout.piece_count() as usize],
        extents: (0..layout.file_count())
            .map(|_| Extents::default())
            .collect(),
        layout,
        dir,
    }
}

fn joined_numbers(values: impl Iterator<Item = u64>) -> String {
    values
        .map(|value| value.to_string())
        .collect::<Vec<_>>()
        .join(",")
}

fn manifest(torrent: &Torrent) -> String {
    let mut result = format!(
        "version=1\npiece_length={}\nfiles={}\npieces={}\n",
        torrent.layout.piece_length(),
        joined_numbers((0..torrent.layout.file_count()).filter_map(|index| {
            torrent
                .layout
                .file_length(FileIndex::from_zero_based(index as u32))
        })),
        joined_numbers(
            torrent
                .completed
                .iter()
                .enumerate()
                .filter_map(|(index, complete)| { complete.then_some(index as u64) })
        )
    );
    for (index, extents) in torrent.extents.iter().enumerate() {
        let spans = extents
            .spans()
            .map(|(start, end)| format!("{start}-{end}"))
            .collect::<Vec<_>>()
            .join(",");
        result.push_str(&format!("extents.{index}={spans}\n"));
    }
    result
}

fn parse_numbers(value: &str) -> Option<Vec<u64>> {
    if value.is_empty() {
        return Some(Vec::new());
    }
    value
        .split(',')
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()
}

fn restore(dir: &Path, layout: &TorrentLayout) -> Option<(Vec<Extents>, Vec<bool>, u64)> {
    let text = fs::read_to_string(dir.join(MANIFEST)).ok()?;
    let mut values = HashMap::new();
    for line in text.lines() {
        let (key, value) = line.split_once('=')?;
        if values.insert(key, value).is_some() {
            return None;
        }
    }
    if values.remove("version")? != "1"
        || values.remove("piece_length")?.parse::<u64>().ok()? != layout.piece_length()
    {
        return None;
    }
    let wanted_files: Vec<u64> = (0..layout.file_count())
        .filter_map(|index| layout.file_length(FileIndex::from_zero_based(index as u32)))
        .collect();
    if parse_numbers(values.remove("files")?)? != wanted_files {
        return None;
    }
    let mut completed = vec![false; layout.piece_count() as usize];
    for piece in parse_numbers(values.remove("pieces")?)? {
        let slot = completed.get_mut(usize::try_from(piece).ok()?)?;
        if std::mem::replace(slot, true) {
            return None;
        }
    }
    let mut stored = 0_u64;
    let mut extents = Vec::with_capacity(layout.file_count());
    for index in 0..layout.file_count() {
        let key = format!("extents.{index}");
        let raw = values.remove(key.as_str())?;
        let spans = if raw.is_empty() {
            Vec::new()
        } else {
            raw.split(',')
                .map(|span| {
                    let (start, end) = span.split_once('-')?;
                    Some((start.parse().ok()?, end.parse().ok()?))
                })
                .collect::<Option<Vec<_>>>()?
        };
        let extents_for_file = Extents::from_spans(spans)?;
        let length = layout.file_length(FileIndex::from_zero_based(index as u32))?;
        if extents_for_file.spans().any(|(_, end)| end > length) {
            return None;
        }
        if extents_for_file.total() > 0 {
            let file_len = fs::metadata(dir.join(index.to_string())).ok()?.len();
            if extents_for_file.spans().any(|(_, end)| end > file_len) {
                return None;
            }
        }
        stored = stored.checked_add(extents_for_file.total())?;
        extents.push(extents_for_file);
    }
    values.is_empty().then_some((extents, completed, stored))
}

fn write_manifest(torrent: &Torrent) -> Result<(), Error> {
    let target = torrent.manifest_path();
    let temporary = target.with_extension("tmp");
    let mut file = File::create(&temporary).map_err(io_error("create cache manifest"))?;
    file.write_all(manifest(torrent).as_bytes())
        .map_err(io_error("write cache manifest"))?;
    file.sync_all().map_err(io_error("sync cache manifest"))?;
    fs::rename(temporary, target).map_err(io_error("replace cache manifest"))
}

impl PieceStore for DiskStore {
    fn open(&self, torrent: InfoHash, layout: &TorrentLayout) -> Result<Recovered, Error> {
        let dir = self.root.join(torrent.to_string());
        // A second explicit open in the same process means the caller asked
        // for fresh storage. Only a new DiskStore instance may recover a
        // previous process's manifest.
        let already_open = self
            .torrents
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .contains_key(&torrent);
        let restored = (!already_open).then(|| restore(&dir, layout)).flatten();
        if restored.is_none() {
            remove_dir(&dir)?;
        }
        fs::create_dir_all(&dir).map_err(io_error("create cache directory"))?;
        let (extents, completed, stored_bytes) = restored.unwrap_or_else(|| {
            let torrent = empty_torrent(dir.clone(), layout.clone());
            (torrent.extents, torrent.completed, 0)
        });
        let created = Arc::new(Mutex::new(Torrent {
            layout: layout.clone(),
            dir,
            extents,
            completed: completed.clone(),
        }));
        self.torrents
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(torrent, created);
        Ok(Recovered {
            stored_bytes,
            completed_pieces: completed
                .into_iter()
                .enumerate()
                .filter_map(|(index, complete)| complete.then_some(PieceIndex::new(index as u32)))
                .collect(),
        })
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
        let added = guard.extents[file.zero_based() as usize].insert(range);
        write_manifest(&guard)?;
        Ok(added)
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

    fn piece_completed(&self, torrent: InfoHash, piece: PieceIndex) -> Result<(), Error> {
        let handle = self.torrent(torrent)?;
        let mut guard = lock(&handle);
        let piece_count = guard.completed.len() as u32;
        let slot = guard
            .completed
            .get_mut(piece.get() as usize)
            .ok_or(Error::PieceOutOfRange { piece, piece_count })?;
        *slot = true;
        write_manifest(&guard)
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

    #[test]
    fn a_valid_manifest_recovers_only_proven_ranges_after_restart() {
        let (_, dir) = store();
        let root = dir.path().join("cache");
        let torrent_id = torrent(7);
        let torrent_layout = layout();
        {
            let store = DiskStore::new(&root);
            store.open(torrent_id, &torrent_layout).unwrap();
            store.write(torrent_id, file(0), 10, b"persisted").unwrap();
            store
                .piece_completed(torrent_id, PieceIndex::new(0))
                .unwrap();
        }

        let recovered = DiskStore::new(&root);
        let snapshot = recovered.open(torrent_id, &torrent_layout).unwrap();
        assert_eq!(snapshot.stored_bytes, 9);
        assert_eq!(snapshot.completed_pieces, [PieceIndex::new(0)]);
        let mut bytes = [0; 9];
        recovered.read(torrent_id, file(0), 10, &mut bytes).unwrap();
        assert_eq!(&bytes, b"persisted");
        assert!(matches!(
            recovered.read(torrent_id, file(0), 0, &mut [0; 1]),
            Err(Error::Missing { .. })
        ));
    }

    #[test]
    fn corrupt_manifest_is_discarded_instead_of_trusting_sparse_data() {
        let (_, dir) = store();
        let root = dir.path().join("cache");
        let torrent_id = torrent(8);
        let torrent_layout = layout();
        let path = root.join(torrent_id.to_string());
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("0"), b"untrusted").unwrap();
        fs::write(path.join(MANIFEST), b"version=broken\n").unwrap();

        let recovered = DiskStore::new(&root);
        let snapshot = recovered.open(torrent_id, &torrent_layout).unwrap();
        assert_eq!(snapshot.stored_bytes, 0);
        assert!(!path.join("0").exists());
    }
}
