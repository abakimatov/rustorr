//! `server/gstreamer/{service,task}.go`: HLS tasks, one per torrent, each
//! owning a pipeline runner; probe results cached for an hour; inactive
//! tasks frozen (their pipeline released) and later removed.
//!
//! Everything here is synchronous: runners block on GStreamer, so callers
//! run these methods on blocking threads.

use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use crate::{
    Config, Error,
    cue::CueTimeline,
    env::ComponentStatus,
    mp4box::Segment,
    mp4init::{self, VariantInfo},
    playlist::{self, Media},
    probe::ProbeInfo,
    subtitles::{self, Store},
};

const PROBE_CACHE_TTL: Duration = Duration::from_secs(3600);
const MAX_SEGMENT_CATCHUP_SECONDS: i64 = 60;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Set when the HTTP request that started an operation went away; loops
/// that would wait for the pipeline give up, as Go's request context does.
#[derive(Debug, Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    pub fn check(&self) -> Result<(), Error> {
        if self.is_cancelled() {
            Err(Error::Canceled)
        } else {
            Ok(())
        }
    }
}

/// `pipelineRunner`: one task's pipeline.
pub trait Runner: Send {
    fn ensure_init(&mut self, cancel: &Cancel, audio: i64, start_index: i64) -> Result<(), Error>;
    fn get_segment(&mut self, cancel: &Cancel, index: i64, audio: i64) -> Result<Segment, Error>;
    fn seek(&mut self, seconds: f64) -> bool;
    fn frozen(&mut self);
    fn dispose(&mut self);
    fn is_frozen(&self) -> bool;
}

/// What the module reads from the rest of the server.
pub trait Host: Send + Sync {
    /// The HTTP port the source URLs point at.
    fn port(&self) -> u16;
    /// The file's length from the torrent's status, if the torrent is loaded
    /// and knows it.
    fn file_size(&self, hash: &str, file_id: &str) -> BoxFuture<'_, Option<i64>>;
    /// `torrentHeartbeatState`: the cache state, or `{"Hash", "Torrent"}`.
    fn heartbeat(&self, hash: &str) -> BoxFuture<'_, serde_json::Value>;
    /// `torr.DropTorrent`.
    fn drop_torrent(&self, hash: &str) -> BoxFuture<'_, ()>;
    /// `gst-discoverer-1.0 -v -t 30 <url>`: its combined output and error.
    fn discover(&self, url: &str, config: &Config) -> BoxFuture<'_, (String, Option<Error>)>;
    /// Reads `length` bytes at `offset` of `url` with a Range request.
    fn read_range(&self, url: &str, offset: u64, length: u64) -> BoxFuture<'_, Option<Vec<u8>>>;
}

/// The GStreamer runtime: creates runners and reports on itself.
pub trait Runtime: Send + Sync {
    fn runner(&self, task: Arc<TaskInfo>, audio: i64) -> Result<Box<dyn Runner>, Error>;
    /// `checkGStreamer`, initialising the runtime on first use.
    fn status(&self, config: &Config) -> ComponentStatus;
    /// The runtime's major.minor as `GSTVersion` reports it.
    fn config_version(&self, config: &Config) -> Option<f64>;
    /// `checkHDRToneMapping`.
    fn hdr_tone_mapping(&self, gstreamer: &ComponentStatus) -> ComponentStatus;
}

/// Where the module's settings are kept (`Settings/gstreamer` in the
/// reference's settings database).
pub trait ConfigStore: Send + Sync {
    fn load(&self) -> Option<String>;
    fn save(&self, document: &str) -> Result<(), String>;
}

/// `/gst/echo`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Echo {
    pub gst_discoverer: ComponentStatus,
    pub gstreamer: ComponentStatus,
    pub hdr_tone_mapping: ComponentStatus,
    pub embedded_runtime: ComponentStatus,
}

/// Where the pipeline reads the file: Rustorr's own `/stream` or `/play`.
pub fn source_url(config: &Config, port: u16, hash: &str, file_id: &str) -> String {
    if config.uses_play_source() {
        format!(
            "http://127.0.0.1:{port}/play/{}/{}",
            path_escape(hash),
            path_escape(file_id)
        )
    } else {
        format!(
            "http://127.0.0.1:{port}/stream/?link={}&index={}&play",
            query_escape(hash),
            query_escape(file_id)
        )
    }
}

fn path_escape(value: &str) -> String {
    escape(value, |byte| {
        byte.is_ascii_alphanumeric()
            || matches!(
                byte,
                b'-' | b'_' | b'.' | b'~' | b'$' | b'&' | b'+' | b':' | b'=' | b'@'
            )
    })
}

/// `url.QueryEscape`: spaces become `+`.
fn query_escape(value: &str) -> String {
    escape(value, |byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~' | b' ')
    })
    .replace(' ', "+")
}

fn escape(value: &str, keep: impl Fn(u8) -> bool) -> String {
    let mut escaped = String::with_capacity(value.len());
    for byte in value.bytes() {
        if keep(byte) {
            escaped.push(byte as char);
        } else {
            escaped.push_str(&format!("%{byte:02X}"));
        }
    }
    escaped
}

/// The init segment and what it says about the variant.
struct Init {
    data: Arc<Vec<u8>>,
    variant: Option<VariantInfo>,
}

/// A task's fixed facts plus the state its runner publishes: the init
/// segment and the subtitle stores.
pub struct TaskInfo {
    pub id: String,
    pub file_id: String,
    pub audio: i64,
    pub source_url: String,
    pub probe: ProbeInfo,
    pub cue: Option<CueTimeline>,
    pub config: Config,
    init: Mutex<Option<Init>>,
    subtitles: Mutex<HashMap<i64, Arc<Store>>>,
}

impl TaskInfo {
    pub fn new(
        id: String,
        file_id: String,
        audio: i64,
        source_url: String,
        probe: ProbeInfo,
        cue: Option<CueTimeline>,
        config: Config,
    ) -> Self {
        Self {
            id,
            file_id,
            audio,
            source_url,
            probe,
            cue,
            config: config.normalized(),
            init: Mutex::new(None),
            subtitles: Mutex::new(HashMap::new()),
        }
    }

    /// `setInitMP4`.
    pub fn set_init(&self, data: Vec<u8>) {
        let mut variant = mp4init::read(&data);
        if let (Some(variant), Some(video)) = (variant.as_mut(), self.probe.video()) {
            if variant.width <= 0 {
                variant.width = video.width;
            }
            if variant.height <= 0 {
                variant.height = video.height;
            }
            if video.frame_rate_num > 0 && video.frame_rate_den > 0 {
                variant.frame_rate = video.frame_rate_num as f64 / video.frame_rate_den as f64;
            }
            if variant.video_range.is_empty() {
                variant.video_range = video.video_transfer.to_uppercase();
            }
            if self.config.hdr_to_sdr && video.is_hdr_video() {
                variant.video_range = "SDR".into();
            }
        }
        *lock(&self.init) = Some(Init {
            data: Arc::new(data),
            variant,
        });
    }

    pub fn clear_init(&self) {
        *lock(&self.init) = None;
    }

    pub fn has_init(&self) -> bool {
        lock(&self.init)
            .as_ref()
            .is_some_and(|init| !init.data.is_empty())
    }

    pub fn init_data(&self) -> Option<Arc<Vec<u8>>> {
        lock(&self.init)
            .as_ref()
            .filter(|init| !init.data.is_empty())
            .map(|init| init.data.clone())
    }

    fn variant(&self) -> Option<VariantInfo> {
        lock(&self.init)
            .as_ref()
            .and_then(|init| init.variant.clone())
    }

    /// `setSubtitleStores`: waiting requests are woken on replacement.
    pub fn set_subtitle_stores(&self, stores: HashMap<i64, Arc<Store>>) {
        let previous = std::mem::replace(&mut *lock(&self.subtitles), stores);
        for store in previous.values() {
            store.notify();
        }
    }

    pub fn subtitle_store(&self, index: i64) -> Option<Arc<Store>> {
        lock(&self.subtitles).get(&index).cloned()
    }

    /// `segmentStartNS`.
    pub fn segment_start_ns(&self, index: i64) -> u64 {
        playlist::segment_start_ns(&self.config, self.cue.as_ref(), index)
    }

    /// `startIndexForSeconds`.
    pub fn start_index_for_seconds(&self, seconds: i64) -> i64 {
        if seconds <= 0 {
            return 0;
        }
        let segment_seconds = self.config.segment_seconds.max(1);
        if let Some(cue) = &self.cue {
            let target = seconds as u64 * 1_000_000_000;
            return cue
                .segments
                .iter()
                .position(|segment| target < segment.end_ns)
                .unwrap_or(cue.segments.len()) as i64;
        }
        let duration = self.probe.duration_seconds();
        let count = if duration > 0 {
            1 + (duration - 1) / segment_seconds
        } else {
            0
        };
        let index = seconds / segment_seconds;
        if count > 0 && index > count {
            count
        } else {
            index
        }
    }

    /// `subtitleRange`.
    pub fn subtitle_range(&self, segment: i64) -> (u64, u64) {
        if let Some(cue) = self.cue.as_ref().and_then(|cue| cue.segment(segment)) {
            return (cue.start_ns, cue.end_ns);
        }
        let from = self.segment_start_ns(segment);
        let mut to = from.saturating_add(self.config.segment_seconds.max(1) as u64 * 1_000_000_000);
        if self.probe.duration_ns > 0 {
            let end = self.probe.duration_ns as u64;
            if from >= end {
                to = from;
            } else if to > end {
                to = end;
            }
        }
        (from, to)
    }

    /// `validateSegmentIndex`.
    fn validate_segment_index(&self, index: i64) -> Result<(), Error> {
        if index < 0 {
            return Err(Error::InvalidIdentifier);
        }
        if let Some(cue) = &self.cue {
            return if index as usize >= cue.segments.len() {
                Err(Error::EndOfStreamExhausted)
            } else {
                Ok(())
            };
        }
        let segment_seconds = self.config.segment_seconds;
        if segment_seconds <= 0 || segment_seconds > i64::MAX / 1_000_000_000 {
            return Err(Error::InvalidIdentifier);
        }
        let segment_ns = segment_seconds * 1_000_000_000;
        if index > i64::MAX / segment_ns {
            return Err(Error::InvalidIdentifier);
        }
        if self.probe.duration_ns > 0 {
            let count = 1 + (self.probe.duration_ns - 1) / segment_ns;
            if index >= count {
                return Err(Error::EndOfStreamExhausted);
            }
        }
        Ok(())
    }

    pub fn media(&self, audio: i64) -> MediaView<'_> {
        MediaView {
            info: self,
            audio,
            variant: self.variant(),
        }
    }
}

/// A task's facts borrowed for playlist building.
pub struct MediaView<'a> {
    info: &'a TaskInfo,
    audio: i64,
    variant: Option<VariantInfo>,
}

impl MediaView<'_> {
    pub fn media(&self) -> Media<'_> {
        Media {
            id: &self.info.id,
            audio: self.audio,
            config: &self.info.config,
            probe: &self.info.probe,
            cue: self.info.cue.as_ref(),
            variant: self.variant.as_ref(),
        }
    }
}

struct TaskState {
    last_sent_segment: i64,
    runner: Option<Box<dyn Runner>>,
}

/// `Task`.
pub struct Task {
    pub info: Arc<TaskInfo>,
    state: Mutex<TaskState>,
    last_active: Mutex<Instant>,
    disposed: AtomicBool,
}

impl Task {
    pub fn new(info: Arc<TaskInfo>, runtime: &dyn Runtime) -> Result<Self, Error> {
        let runner = runtime.runner(info.clone(), info.audio)?;
        Ok(Self {
            info,
            state: Mutex::new(TaskState {
                last_sent_segment: -1,
                runner: Some(runner),
            }),
            last_active: Mutex::new(Instant::now()),
            disposed: AtomicBool::new(false),
        })
    }

    pub fn update_last_active(&self) {
        *lock(&self.last_active) = Instant::now();
    }

    pub fn last_active(&self) -> Instant {
        *lock(&self.last_active)
    }

    pub fn is_disposed(&self) -> bool {
        self.disposed.load(Ordering::SeqCst)
    }

    /// `EnsureInit`.
    pub fn ensure_init(&self, cancel: &Cancel, audio: i64, start_index: i64) -> Result<(), Error> {
        let mut state = lock(&self.state);
        let start_index = start_index.max(0);
        self.info.validate_segment_index(start_index)?;
        if self.info.has_init() && (start_index == 0 || state.last_sent_segment != -1) {
            return Ok(());
        }
        let last_sent = state.last_sent_segment;
        let Some(runner) = state.runner.as_mut() else {
            return Err(Error::TaskNotFound);
        };
        let result = runner.ensure_init(cancel, audio, start_index);
        if result.is_ok() && start_index > 0 && last_sent == -1 {
            state.last_sent_segment = start_index - 1;
        }
        result
    }

    /// `WithSegment`: the segment, fetched in order where possible.
    pub fn segment(&self, cancel: &Cancel, index: i64, audio: i64) -> Result<Segment, Error> {
        let mut state = lock(&self.state);
        let segment = self.segment_locked(&mut state, cancel, index, audio)?;
        cancel.check()?;
        Ok(segment)
    }

    fn segment_locked(
        &self,
        state: &mut TaskState,
        cancel: &Cancel,
        index: i64,
        audio: i64,
    ) -> Result<Segment, Error> {
        if state.runner.is_none() {
            return Err(Error::TaskNotFound);
        }
        self.info.validate_segment_index(index)?;
        let frozen = state
            .runner
            .as_ref()
            .is_some_and(|runner| runner.is_frozen());
        let last = state.last_sent_segment;
        if frozen || (last == -1 && index > 0) {
            self.seek_to_segment(state, index)?;
        } else if last != -1 && last != index && index != last + 1 {
            let mut seek_required = true;
            if self.info.cue.is_none() {
                let diff = index - last;
                if diff > 0
                    && diff <= MAX_SEGMENT_CATCHUP_SECONDS / self.info.config.segment_seconds
                {
                    for _ in 0..diff - 1 {
                        cancel.check()?;
                        state.last_sent_segment += 1;
                        let next = state.last_sent_segment;
                        let runner = state.runner.as_mut().ok_or(Error::TaskNotFound)?;
                        if let Err(error) = runner.get_segment(cancel, next, audio) {
                            state.last_sent_segment -= 1;
                            return Err(error);
                        }
                    }
                    seek_required = false;
                }
            }
            if seek_required {
                self.seek_to_segment(state, index)?;
            }
        }
        let runner = state.runner.as_mut().ok_or(Error::TaskNotFound)?;
        let segment = runner.get_segment(cancel, index, audio)?;
        state.last_sent_segment = index;
        Ok(segment)
    }

    fn seek_to_segment(&self, state: &mut TaskState, index: i64) -> Result<(), Error> {
        let runner = state.runner.as_mut().ok_or(Error::TaskNotFound)?;
        let seconds = if let Some(cue) = &self.info.cue {
            let Some(segment) = cue.segment(index) else {
                return Err(Error::EndOfStreamExhausted);
            };
            segment.start_ns as f64 / 1_000_000_000.0
        } else {
            index as f64 * self.info.config.segment_seconds as f64
        };
        if runner.seek(seconds) {
            Ok(())
        } else {
            Err(Error::SegmentNotReady)
        }
    }

    /// `FreezeIfInactive`.
    fn freeze_if_inactive(&self, cutoff: Instant) -> bool {
        let mut state = lock(&self.state);
        if self.last_active() >= cutoff {
            return false;
        }
        if self.is_disposed() {
            return false;
        }
        let Some(runner) = state.runner.as_mut() else {
            return false;
        };
        if runner.is_frozen() {
            return false;
        }
        runner.frozen();
        self.info.clear_init();
        true
    }

    /// `Dispose`.
    pub fn dispose(&self) {
        if self.disposed.swap(true, Ordering::SeqCst) {
            return;
        }
        let mut state = lock(&self.state);
        if let Some(mut runner) = state.runner.take() {
            runner.dispose();
        }
        self.info.clear_init();
    }

    /// `WaitSubtitleVTT`: the segment's cues once the video has been read
    /// past it, or whatever is known when `timeout` runs out.
    pub async fn subtitle_vtt(&self, track: i64, segment: i64, timeout: Duration) -> String {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let (from, to) = self.info.subtitle_range(segment);
            let store = if self.is_disposed() {
                None
            } else {
                self.info.subtitle_store(track)
            };
            let Some(store) = store else {
                return subtitles::header(from);
            };
            let mut updates = store.subscribe();
            let (value, ready) = store.render(from, to);
            if ready {
                return value;
            }
            tokio::select! {
                () = tokio::time::sleep_until(deadline) => {
                    let (from, to) = self.info.subtitle_range(segment);
                    return match self.info.subtitle_store(track) {
                        Some(store) if !self.is_disposed() => store.render(from, to).0,
                        _ => subtitles::header(from),
                    };
                }
                _ = updates.changed() => {}
            }
        }
    }
}

struct CachedProbe {
    probe: ProbeInfo,
    expires: Instant,
}

/// `Service`.
pub struct Service {
    store: Arc<dyn ConfigStore>,
    config: Mutex<Config>,
    tasks: Mutex<HashMap<String, Arc<Task>>>,
    probes: Mutex<HashMap<(String, String), CachedProbe>>,
    probe_calls: tokio::sync::Mutex<()>,
    task_calls: tokio::sync::Mutex<()>,
    disposed: AtomicBool,
    host: Arc<dyn Host>,
    runtime: Arc<dyn Runtime>,
}

impl Service {
    /// `NewService(DefaultConfig())`: the platform defaults with the stored
    /// fields applied.
    pub fn new(
        store: Arc<dyn ConfigStore>,
        host: Arc<dyn Host>,
        runtime: Arc<dyn Runtime>,
    ) -> Arc<Self> {
        let mut config = Config::platform_defaults();
        if let Some(document) = store.load() {
            config = config.with_stored(&document);
        }
        Arc::new(Self {
            store,
            config: Mutex::new(config.normalized()),
            tasks: Mutex::new(HashMap::new()),
            probes: Mutex::new(HashMap::new()),
            probe_calls: tokio::sync::Mutex::new(()),
            task_calls: tokio::sync::Mutex::new(()),
            disposed: AtomicBool::new(false),
            host,
            runtime,
        })
    }

    pub fn host(&self) -> &dyn Host {
        self.host.as_ref()
    }

    pub fn config(&self) -> Config {
        lock(&self.config).clone()
    }

    /// `CurrentConfig`: the settings with the runtime's version.
    pub fn current_config(&self) -> Config {
        let mut config = self.config();
        if let Some(version) = self.runtime.config_version(&config) {
            config.gst_version = version;
        }
        config
    }

    /// `UpdateConfig`: saved normalized, then applied.
    pub fn save_config(&self, config: Config) -> Result<(), String> {
        let config = config.normalized();
        self.store.save(&config.stored_document())?;
        self.update_config(config);
        Ok(())
    }

    /// `/gst/echo`.
    pub async fn echo(&self) -> Echo {
        let config = self.config();
        let runtime = self.runtime.clone();
        let status_config = config.clone();
        let gstreamer = tokio::task::spawn_blocking(move || runtime.status(&status_config))
            .await
            .unwrap_or_default();
        let hdr_tone_mapping = self.runtime.hdr_tone_mapping(&gstreamer);
        Echo {
            gst_discoverer: crate::env::discoverer_status(&config).await,
            gstreamer,
            hdr_tone_mapping,
            embedded_runtime: ComponentStatus::default(),
        }
    }

    /// `updateConfig`: applies to new tasks; evicts beyond `MaxTasks`.
    pub fn update_config(&self, config: Config) {
        if self.disposed.load(Ordering::SeqCst) {
            return;
        }
        *lock(&self.config) = config.normalized();
        let evicted = self.evict_for_limit(None);
        for task in evicted {
            dispose_blocking(task);
        }
    }

    /// `Get`.
    pub fn get(&self, id: &str) -> Option<Arc<Task>> {
        if id.is_empty() || self.disposed.load(Ordering::SeqCst) {
            return None;
        }
        let task = lock(&self.tasks).get(id).cloned()?;
        if task.is_disposed() {
            return None;
        }
        task.update_last_active();
        Some(task)
    }

    /// `TryRemove`.
    pub fn try_remove(&self, id: &str) -> bool {
        if id.is_empty() {
            return false;
        }
        let Some(task) = lock(&self.tasks).remove(id) else {
            return false;
        };
        dispose_blocking(task);
        true
    }

    /// `Dispose`.
    pub fn dispose(&self) {
        if self.disposed.swap(true, Ordering::SeqCst) {
            return;
        }
        let tasks = std::mem::take(&mut *lock(&self.tasks));
        lock(&self.probes).clear();
        for task in tasks.into_values() {
            dispose_blocking(task);
        }
    }

    /// `GetOrAdd`: the torrent's task for this file and audio track, created
    /// (probe, cue timeline, runner) if there is none.
    pub async fn get_or_add(
        &self,
        hash: &str,
        file_id: &str,
        audio: i64,
    ) -> Result<Arc<Task>, Error> {
        if hash.is_empty() || file_id.is_empty() {
            return Err(Error::BadSource);
        }
        if self.disposed.load(Ordering::SeqCst) {
            return Err(Error::ServiceClosed);
        }
        if let Some(task) = self.matching_task(hash, file_id, audio) {
            return Ok(task);
        }
        let _creating = self.task_calls.lock().await;
        if let Some(task) = self.matching_task(hash, file_id, audio) {
            return Ok(task);
        }
        let config = self.config();
        let url = source_url(&config, self.host.port(), hash, file_id);
        let probe = self.probe(hash, file_id).await?;
        let cue = if use_cue_timeline(&config, &probe) {
            let host = self.host.clone();
            let read = |offset, length| {
                let host = host.clone();
                let url = url.clone();
                async move { host.read_range(&url, offset, length).await }
            };
            tokio::time::timeout(
                crate::cue::READ_TIMEOUT,
                crate::cue::read_timeline(probe.file_size, probe.duration_ns, read),
            )
            .await
            .ok()
            .flatten()
        } else {
            None
        };
        let info = Arc::new(TaskInfo::new(
            hash.into(),
            file_id.into(),
            audio,
            url,
            probe,
            cue,
            config,
        ));
        let runtime = self.runtime.clone();
        let task = tokio::task::spawn_blocking(move || Task::new(info, runtime.as_ref()))
            .await
            .map_err(|error| Error::Other(error.to_string()))??;
        let task = Arc::new(task);
        if self.disposed.load(Ordering::SeqCst) {
            dispose_blocking(task);
            return Err(Error::ServiceClosed);
        }
        let replaced = lock(&self.tasks).insert(hash.into(), task.clone());
        if let Some(replaced) = replaced {
            dispose_blocking(replaced);
        }
        for evicted in self.evict_for_limit(Some(hash)) {
            dispose_blocking(evicted);
        }
        Ok(task)
    }

    fn matching_task(&self, hash: &str, file_id: &str, audio: i64) -> Option<Arc<Task>> {
        let task = lock(&self.tasks).get(hash).cloned()?;
        let matches =
            task.info.file_id == file_id && task.info.audio == audio && !task.is_disposed();
        matches.then(|| {
            task.update_last_active();
            task
        })
    }

    fn evict_for_limit(&self, protected: Option<&str>) -> Vec<Arc<Task>> {
        let limit = lock(&self.config).max_tasks;
        let mut tasks = lock(&self.tasks);
        let mut evicted = Vec::new();
        if limit <= 0 {
            return evicted;
        }
        while tasks.len() > limit as usize {
            let oldest = tasks
                .iter()
                .filter(|(id, _)| Some(id.as_str()) != protected)
                .min_by_key(|(_, task)| (!task.is_disposed(), task.last_active()))
                .map(|(id, _)| id.clone());
            let Some(id) = oldest else {
                break;
            };
            evicted.extend(tasks.remove(&id));
        }
        evicted
    }

    /// `Probe`: discoverer's view of the file, cached for an hour, with the
    /// size refreshed from the torrent on every use.
    pub async fn probe(&self, hash: &str, file_id: &str) -> Result<ProbeInfo, Error> {
        if hash.is_empty() || file_id.is_empty() {
            return Err(Error::BadSource);
        }
        if self.disposed.load(Ordering::SeqCst) {
            return Err(Error::ServiceClosed);
        }
        let key = (hash.to_string(), file_id.to_string());
        let cached = self.cached_probe(&key);
        let probe = match cached {
            Some(probe) => probe,
            None => {
                let _probing = self.probe_calls.lock().await;
                match self.cached_probe(&key) {
                    Some(probe) => probe,
                    None => {
                        let config = self.config();
                        let url = source_url(&config, self.host.port(), hash, file_id);
                        let (output, error) = self.host.discover(&url, &config).await;
                        probe_output(&output, error)?
                    }
                }
            }
        };
        let probe = self.refresh_file_size(probe, hash, file_id).await;
        probe.validate(&self.config())?;
        if self.disposed.load(Ordering::SeqCst) {
            return Err(Error::ServiceClosed);
        }
        lock(&self.probes).insert(
            key,
            CachedProbe {
                probe: probe.clone(),
                expires: Instant::now() + PROBE_CACHE_TTL,
            },
        );
        Ok(probe)
    }

    fn cached_probe(&self, key: &(String, String)) -> Option<ProbeInfo> {
        let mut probes = lock(&self.probes);
        let entry = probes.get(key)?;
        if Instant::now() >= entry.expires {
            probes.remove(key);
            return None;
        }
        Some(entry.probe.clone())
    }

    async fn refresh_file_size(
        &self,
        mut probe: ProbeInfo,
        hash: &str,
        file_id: &str,
    ) -> ProbeInfo {
        if let Some(size) = self.host.file_size(hash, file_id).await
            && size > 0
        {
            probe.file_size = size;
        }
        probe
    }

    /// `cleanupInactive`, run every minute: tasks idle for `InactiveMinutes`
    /// release their pipeline, and twenty minutes later they are removed.
    pub fn cleanup_inactive(&self) {
        if self.disposed.load(Ordering::SeqCst) {
            return;
        }
        let now = Instant::now();
        let inactive = Duration::from_secs(self.config().inactive_minutes.max(1) as u64 * 60);
        let freeze_cutoff = now.checked_sub(inactive);
        let remove_cutoff = now.checked_sub(inactive + Duration::from_secs(20 * 60));
        let snapshot: Vec<(String, Arc<Task>)> = lock(&self.tasks)
            .iter()
            .map(|(id, task)| (id.clone(), task.clone()))
            .collect();
        for (id, task) in snapshot {
            let last_active = task.last_active();
            if let Some(cutoff) = remove_cutoff
                && last_active < cutoff
            {
                let removed = {
                    let mut tasks = lock(&self.tasks);
                    let current = tasks
                        .get(&id)
                        .is_some_and(|current| Arc::ptr_eq(current, &task));
                    current && task.last_active() < cutoff && tasks.remove(&id).is_some()
                };
                if removed {
                    task.dispose();
                }
                continue;
            }
            if let Some(cutoff) = freeze_cutoff
                && last_active < cutoff
                && lock(&self.tasks)
                    .get(&id)
                    .is_some_and(|current| Arc::ptr_eq(current, &task))
            {
                task.freeze_if_inactive(cutoff);
            }
        }
        lock(&self.probes).retain(|_, entry| now < entry.expires);
    }
}

/// Disposing stops a pipeline, which may block; off the async runtime when
/// there is one.
fn dispose_blocking(task: Arc<Task>) {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            handle.spawn_blocking(move || task.dispose());
        }
        Err(_) => task.dispose(),
    }
}

/// `probeSource`: discoverer's output parsed; an empty output or one without
/// streams is an error.
fn probe_output(output: &str, error: Option<Error>) -> Result<ProbeInfo, Error> {
    if output.trim().is_empty() {
        return Err(
            error.unwrap_or_else(|| Error::Other("gst-discoverer returned no output".into()))
        );
    }
    let probe = crate::probe::from_discoverer(output);
    if probe.tracks().is_empty() {
        return Err(match error {
            Some(error) => Error::Other(format!("gst-discoverer parse failed: {error}")),
            None => Error::ProbeUnavailable,
        });
    }
    Ok(probe)
}

/// `shouldUseCueTimeline`: only a remuxed Matroska video follows its cues.
fn use_cue_timeline(config: &Config, probe: &ProbeInfo) -> bool {
    if !probe.is_matroska_container() {
        return false;
    }
    if probe.is_h264() {
        !config.transcode_h264
    } else if probe.is_h265() {
        !config.transcode_h265
    } else if probe.is_av1() {
        !config.transcode_av1
    } else if probe.is_vp9() {
        !config.transcode_vp9
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sources_point_at_the_servers_own_routes() {
        let config = Config::platform_defaults();
        assert_eq!(
            source_url(&config, 8090, "ab cd", "1"),
            "http://127.0.0.1:8090/stream/?link=ab+cd&index=1&play"
        );
        let play = Config {
            source: "play".into(),
            ..config
        };
        assert_eq!(
            source_url(&play, 8090, "ab cd", "1"),
            "http://127.0.0.1:8090/play/ab%20cd/1"
        );
    }

    #[test]
    fn discoverer_failures_keep_the_reference_texts() {
        assert_eq!(
            probe_output(" \n", None).unwrap_err().to_string(),
            "gst-discoverer returned no output"
        );
        assert_eq!(
            probe_output(
                "Properties:\n  container: WAV\n",
                Some(Error::Other("exit status 1".into()))
            )
            .unwrap_err()
            .to_string(),
            "gst-discoverer parse failed: exit status 1"
        );
        assert_eq!(
            probe_output("Properties:\n", None).unwrap_err(),
            Error::ProbeUnavailable
        );
    }
}
