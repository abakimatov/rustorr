use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

use rustorr_domain::{FileIndex, InfoHash, PieceIndex};

use crate::{Error, PieceStore, TorrentLayout, layout::PieceSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheConfig {
    /// Total bytes the cache tries to stay under. It is a target, not a hard
    /// limit: a torrent with a live view is never evicted, so a single watched
    /// torrent can exceed it.
    pub cap_bytes: u64,
}

impl CacheConfig {
    /// A placeholder until R5 sizes the cap against measurements (ADR 0004).
    pub const PROVISIONAL_CAP_BYTES: u64 = 4 << 30;
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            cap_bytes: Self::PROVISIONAL_CAP_BYTES,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheStats {
    pub stored_bytes: u64,
    pub cap_bytes: u64,
    pub torrents: usize,
    pub pinned_torrents: usize,
}

impl CacheStats {
    pub fn over_cap_bytes(&self) -> u64 {
        self.stored_bytes.saturating_sub(self.cap_bytes)
    }
}

/// The Rustorr-owned cache (ADR 0004): torrent-scoped LRU over a
/// [`PieceStore`], with a residency index of verified pieces.
///
/// Eviction is two-phase and driven from outside, because removing data under
/// a torrent the engine still holds makes reads fail. The cache only *names*
/// victims with [`Cache::eviction_candidates`]; the owner of the torrent
/// lifecycle stops and deletes each one in the engine and then calls
/// [`Cache::remove`]. The cache never calls the engine.
pub struct Cache {
    store: Arc<dyn PieceStore>,
    cap_bytes: u64,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    tick: u64,
    torrents: HashMap<InfoHash, Entry>,
}

struct Entry {
    layout: TorrentLayout,
    pieces: PieceSet,
    stored: u64,
    pins: usize,
    last_read: u64,
}

impl State {
    /// Recency is a logical clock, not wall time, so ordering is exact.
    fn next_tick(&mut self) -> u64 {
        self.tick += 1;
        self.tick
    }

    fn entry(&mut self, torrent: InfoHash) -> Result<&mut Entry, Error> {
        self.torrents
            .get_mut(&torrent)
            .ok_or(Error::UnknownTorrent(torrent))
    }
}

impl Cache {
    pub fn new(store: Arc<dyn PieceStore>, config: CacheConfig) -> Self {
        Self {
            store,
            cap_bytes: config.cap_bytes,
            state: Mutex::default(),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Registers a torrent, or attaches to it if it is already open with the
    /// same layout, keeping what is stored. The engine creates a torrent's
    /// storage again on every pause and resume, so this must not reset data.
    pub fn open(&self, torrent: InfoHash, layout: TorrentLayout) -> Result<(), Error> {
        let mut state = self.state();
        if let Some(entry) = state.torrents.get(&torrent) {
            return if entry.layout == layout {
                Ok(())
            } else {
                Err(Error::LayoutMismatch(torrent))
            };
        }
        let recovered = self.store.open(torrent, &layout)?;
        let last_read = state.next_tick();
        let mut pieces = PieceSet::new(layout.piece_count());
        for piece in recovered.completed_pieces {
            pieces.insert(piece)?;
        }
        state.torrents.insert(
            torrent,
            Entry {
                pieces,
                layout,
                stored: recovered.stored_bytes,
                pins: 0,
                last_read,
            },
        );
        Ok(())
    }

    pub fn write(
        &self,
        torrent: InfoHash,
        file: FileIndex,
        offset: u64,
        data: &[u8],
    ) -> Result<(), Error> {
        self.state().entry(torrent)?;
        // Store IO runs outside the lock so torrents do not block each other.
        let added = self.store.write(torrent, file, offset, data)?;
        if let Some(entry) = self.state().torrents.get_mut(&torrent) {
            entry.stored += added;
        }
        Ok(())
    }

    /// Reads stored bytes and marks the torrent as recently read. Bytes that
    /// are not stored are an error, never zeros.
    pub fn read(
        &self,
        torrent: InfoHash,
        file: FileIndex,
        offset: u64,
        buf: &mut [u8],
    ) -> Result<(), Error> {
        {
            let mut state = self.state();
            let tick = state.next_tick();
            state.entry(torrent)?.last_read = tick;
        }
        self.store.read(torrent, file, offset, buf)
    }

    pub fn file_length(&self, torrent: InfoHash, file: FileIndex) -> Result<u64, Error> {
        self.state()
            .entry(torrent)?
            .layout
            .file_length(file)
            .ok_or(Error::UnknownFile { torrent, file })
    }

    /// Records that the engine completed and verified a piece.
    pub fn piece_completed(&self, torrent: InfoHash, piece: PieceIndex) -> Result<(), Error> {
        self.store.piece_completed(torrent, piece)?;
        self.state().entry(torrent)?.pieces.insert(piece)
    }

    pub fn is_piece_resident(&self, torrent: InfoHash, piece: PieceIndex) -> Result<bool, Error> {
        Ok(self.state().entry(torrent)?.pieces.contains(piece))
    }

    /// Marks the torrent as being watched. While any [`Pin`] is alive the
    /// torrent cannot be evicted.
    pub fn pin(self: &Arc<Self>, torrent: InfoHash) -> Result<Pin, Error> {
        self.state().entry(torrent)?.pins += 1;
        Ok(Pin {
            cache: Arc::clone(self),
            torrent,
        })
    }

    /// Validates the lifecycle precondition for an engine delete without
    /// changing cache state. The coordinator calls this before touching the
    /// engine so a live playback can never cause a partial delete.
    pub fn ensure_unpinned(&self, torrent: InfoHash) -> Result<(), Error> {
        if self.state().entry(torrent)?.pins > 0 {
            return Err(Error::Pinned(torrent));
        }
        Ok(())
    }

    /// Torrents to evict, least recently read first, until the cache would be
    /// back under its cap. Pinned torrents and torrents holding nothing are
    /// never named, so the list can fall short of the excess.
    pub fn eviction_candidates(&self) -> Vec<InfoHash> {
        let state = self.state();
        let stored: u64 = state.torrents.values().map(|entry| entry.stored).sum();
        let mut excess = stored.saturating_sub(self.cap_bytes);

        let mut idle: Vec<_> = state
            .torrents
            .iter()
            .filter(|(_, entry)| entry.pins == 0 && entry.stored > 0)
            .collect();
        idle.sort_by_key(|(_, entry)| entry.last_read);

        let mut victims = Vec::new();
        for (&torrent, entry) in idle {
            if excess == 0 {
                break;
            }
            victims.push(torrent);
            excess = excess.saturating_sub(entry.stored);
        }
        victims
    }

    /// Drops a torrent and everything stored for it, returning the bytes
    /// freed. Only call this after the engine has released the torrent.
    /// Refused while the torrent is pinned; a failure in the store leaves the
    /// torrent registered.
    pub fn remove(&self, torrent: InfoHash) -> Result<u64, Error> {
        let mut state = self.state();
        if state.entry(torrent)?.pins > 0 {
            return Err(Error::Pinned(torrent));
        }
        self.store.remove(torrent)?;
        Ok(state
            .torrents
            .remove(&torrent)
            .map_or(0, |entry| entry.stored))
    }

    pub fn stats(&self) -> CacheStats {
        let state = self.state();
        CacheStats {
            stored_bytes: state.torrents.values().map(|entry| entry.stored).sum(),
            cap_bytes: self.cap_bytes,
            torrents: state.torrents.len(),
            pinned_torrents: state.torrents.values().filter(|e| e.pins > 0).count(),
        }
    }
}

/// A live view of a torrent. Dropping it unpins the torrent and counts as a
/// read for recency.
#[must_use = "the torrent is only protected from eviction while the pin is held"]
pub struct Pin {
    cache: Arc<Cache>,
    torrent: InfoHash,
}

impl Drop for Pin {
    fn drop(&mut self) {
        let mut state = self.cache.state();
        let tick = state.next_tick();
        if let Some(entry) = state.torrents.get_mut(&self.torrent) {
            entry.pins -= 1;
            entry.last_read = tick;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;
    use crate::{
        MemoryStore,
        testing::{file, layout, torrent},
    };

    /// Accepts everything and records removals, so policy tests need no bytes.
    #[derive(Default)]
    struct FakeStore {
        removed: Mutex<Vec<InfoHash>>,
        fail_removal: AtomicBool,
    }

    impl PieceStore for FakeStore {
        fn open(&self, _: InfoHash, _: &TorrentLayout) -> Result<crate::Recovered, Error> {
            Ok(crate::Recovered::default())
        }

        fn write(&self, _: InfoHash, _: FileIndex, _: u64, data: &[u8]) -> Result<u64, Error> {
            Ok(data.len() as u64)
        }

        fn read(&self, _: InfoHash, _: FileIndex, _: u64, _: &mut [u8]) -> Result<(), Error> {
            Ok(())
        }

        fn remove(&self, torrent: InfoHash) -> Result<(), Error> {
            if self.fail_removal.load(Ordering::Relaxed) {
                return Err(Error::Io {
                    context: "remove",
                    source: std::io::ErrorKind::PermissionDenied.into(),
                });
            }
            self.removed.lock().unwrap().push(torrent);
            Ok(())
        }
    }

    fn cache_with(cap_bytes: u64) -> (Arc<Cache>, Arc<FakeStore>) {
        let store = Arc::new(FakeStore::default());
        let cache = Arc::new(Cache::new(store.clone(), CacheConfig { cap_bytes }));
        (cache, store)
    }

    /// Opens torrents 1..=n, each holding 40 bytes, oldest first.
    fn fill(cache: &Cache, n: u8) {
        for t in 1..=n {
            cache.open(torrent(t), layout()).unwrap();
            cache.write(torrent(t), file(0), 0, &[0; 40]).unwrap();
        }
    }

    #[test]
    fn nothing_is_evicted_while_under_the_cap() {
        let (cache, _) = cache_with(120);
        fill(&cache, 3);

        assert!(cache.eviction_candidates().is_empty());
        assert_eq!(cache.stats().over_cap_bytes(), 0);
    }

    #[test]
    fn the_least_recently_read_torrent_goes_first_and_only_as_many_as_needed() {
        let (cache, _) = cache_with(100);
        fill(&cache, 3);
        assert_eq!(cache.eviction_candidates(), [torrent(1)]);

        let mut buf = [0; 1];
        cache.read(torrent(1), file(0), 0, &mut buf).unwrap();
        assert_eq!(cache.eviction_candidates(), [torrent(2)]);

        let (cache, _) = cache_with(50);
        fill(&cache, 3);
        assert_eq!(cache.eviction_candidates(), [torrent(1), torrent(2)]);
    }

    #[test]
    fn writes_do_not_refresh_recency() {
        let (cache, _) = cache_with(100);
        fill(&cache, 3);

        cache.write(torrent(1), file(0), 100, &[0; 1]).unwrap();

        assert_eq!(cache.eviction_candidates(), [torrent(1)]);
    }

    #[test]
    fn a_pinned_torrent_is_never_a_candidate() {
        let (cache, _) = cache_with(100);
        fill(&cache, 3);

        let pin = cache.pin(torrent(1)).unwrap();

        assert_eq!(cache.eviction_candidates(), [torrent(2)]);
        drop(pin);
    }

    #[test]
    fn dropping_a_pin_makes_the_torrent_the_most_recently_used() {
        let (cache, _) = cache_with(100);
        fill(&cache, 3);

        drop(cache.pin(torrent(1)).unwrap());

        assert_eq!(cache.eviction_candidates(), [torrent(2)]);
    }

    #[test]
    fn when_everything_is_pinned_the_cache_stays_over_its_cap() {
        let (cache, _) = cache_with(50);
        fill(&cache, 3);
        let pins: Vec<_> = (1..=3).map(|t| cache.pin(torrent(t)).unwrap()).collect();

        assert!(cache.eviction_candidates().is_empty());
        let stats = cache.stats();
        assert_eq!((stats.stored_bytes, stats.pinned_torrents), (120, 3));
        assert_eq!(stats.over_cap_bytes(), 70);
        drop(pins);
    }

    #[test]
    fn torrents_holding_nothing_are_not_worth_evicting() {
        let (cache, _) = cache_with(30);
        cache.open(torrent(1), layout()).unwrap();
        fill_one(&cache, 2, 40);

        assert_eq!(cache.eviction_candidates(), [torrent(2)]);
    }

    fn fill_one(cache: &Cache, t: u8, len: usize) {
        cache.open(torrent(t), layout()).unwrap();
        cache.write(torrent(t), file(0), 0, &vec![0; len]).unwrap();
    }

    #[test]
    fn remove_frees_the_torrent_and_reports_the_bytes() {
        let (cache, store) = cache_with(100);
        fill(&cache, 3);

        assert_eq!(cache.remove(torrent(1)).unwrap(), 40);

        assert_eq!(*store.removed.lock().unwrap(), [torrent(1)]);
        assert_eq!(cache.stats().stored_bytes, 80);
        assert_eq!(cache.stats().torrents, 2);
        assert!(matches!(
            cache.remove(torrent(1)),
            Err(Error::UnknownTorrent(_))
        ));
    }

    #[test]
    fn remove_refuses_a_pinned_torrent_until_the_pin_is_dropped() {
        let (cache, store) = cache_with(100);
        fill(&cache, 1);
        let pin = cache.pin(torrent(1)).unwrap();

        assert!(matches!(cache.remove(torrent(1)), Err(Error::Pinned(_))));
        assert!(store.removed.lock().unwrap().is_empty());

        drop(pin);
        assert_eq!(cache.remove(torrent(1)).unwrap(), 40);
    }

    #[test]
    fn a_failed_store_removal_keeps_the_torrent_registered() {
        let (cache, store) = cache_with(100);
        fill(&cache, 1);
        store.fail_removal.store(true, Ordering::Relaxed);

        assert!(matches!(cache.remove(torrent(1)), Err(Error::Io { .. })));

        assert_eq!(cache.stats().stored_bytes, 40);
        store.fail_removal.store(false, Ordering::Relaxed);
        assert_eq!(cache.remove(torrent(1)).unwrap(), 40);
    }

    #[test]
    fn reopening_with_the_same_layout_keeps_what_is_stored() {
        let (cache, _) = cache_with(100);
        fill(&cache, 1);

        cache.open(torrent(1), layout()).unwrap();

        assert_eq!(cache.stats().stored_bytes, 40);
    }

    #[test]
    fn reopening_with_another_layout_is_refused() {
        let (cache, _) = cache_with(100);
        fill(&cache, 1);
        let other = TorrentLayout::new(1024, vec![10]).unwrap();

        assert!(matches!(
            cache.open(torrent(1), other),
            Err(Error::LayoutMismatch(_))
        ));
    }

    #[test]
    fn unknown_torrents_are_reported_everywhere() {
        let (cache, _) = cache_with(100);
        let mut buf = [0; 1];

        assert!(matches!(
            cache.write(torrent(1), file(0), 0, &[0]),
            Err(Error::UnknownTorrent(_))
        ));
        assert!(matches!(
            cache.read(torrent(1), file(0), 0, &mut buf),
            Err(Error::UnknownTorrent(_))
        ));
        assert!(matches!(
            cache.pin(torrent(1)),
            Err(Error::UnknownTorrent(_))
        ));
        assert!(matches!(
            cache.file_length(torrent(1), file(0)),
            Err(Error::UnknownTorrent(_))
        ));
    }

    #[test]
    fn residency_follows_completed_pieces() {
        let (cache, _) = cache_with(100);
        cache.open(torrent(1), layout()).unwrap();
        let piece = PieceIndex::new(1);

        assert!(!cache.is_piece_resident(torrent(1), piece).unwrap());
        cache.piece_completed(torrent(1), piece).unwrap();

        assert!(cache.is_piece_resident(torrent(1), piece).unwrap());
        assert!(
            !cache
                .is_piece_resident(torrent(1), PieceIndex::new(0))
                .unwrap()
        );
        assert!(matches!(
            cache.piece_completed(torrent(1), PieceIndex::new(3)),
            Err(Error::PieceOutOfRange { piece_count: 3, .. })
        ));
    }

    #[test]
    fn file_length_comes_from_the_layout() {
        let (cache, _) = cache_with(100);
        cache.open(torrent(1), layout()).unwrap();

        assert_eq!(cache.file_length(torrent(1), file(1)).unwrap(), 50_000);
        assert!(matches!(
            cache.file_length(torrent(1), file(2)),
            Err(Error::UnknownFile { .. })
        ));
    }

    #[test]
    fn removing_a_torrent_the_engine_still_reads_fails_loudly() {
        let cache = Cache::new(Arc::new(MemoryStore::new()), CacheConfig::default());
        cache.open(torrent(1), layout()).unwrap();
        cache.write(torrent(1), file(0), 0, &[9; 64]).unwrap();
        let mut buf = [0; 64];
        cache.read(torrent(1), file(0), 0, &mut buf).unwrap();
        assert_eq!(buf, [9; 64]);

        cache.remove(torrent(1)).unwrap();

        let error = cache.read(torrent(1), file(0), 0, &mut buf).unwrap_err();
        assert!(matches!(error, Error::UnknownTorrent(_)), "{error}");
        assert_eq!(
            buf, [9; 64],
            "the buffer must not be overwritten with zeros"
        );
    }

    #[test]
    fn bytes_never_written_are_an_error_through_the_cache_too() {
        let cache = Cache::new(Arc::new(MemoryStore::new()), CacheConfig::default());
        cache.open(torrent(1), layout()).unwrap();
        let mut buf = [0; 8];

        assert!(matches!(
            cache.read(torrent(1), file(0), 0, &mut buf),
            Err(Error::Missing { .. })
        ));
    }

    #[test]
    fn the_default_cap_is_the_provisional_one() {
        assert_eq!(CacheConfig::default().cap_bytes, 4 * 1024 * 1024 * 1024);
    }
}
