//! `server/gstreamer/{pipeline_gst,video_probe_gst,hardware_gst}.go`: the
//! pipeline runner on gstreamer-rs. The reference drives the same GStreamer
//! C API through purego; the pipeline descriptions, seek flags, pad probes
//! and timeouts are the same.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

use crate::{
    Config, Error,
    env::{self, ComponentStatus},
    mp4box::{Reader, Segment},
    playlist::{aac_channels, video_is_transcoded},
    probe::{ProbeInfo, TrackInfo},
    service::{Cancel, Runner, Runtime, TaskInfo},
    subtitles::{Store, supported_track},
};

const DEFAULT_AAC_SAMPLE_RATE: i64 = 48_000;
const READ_TIMEOUT: Duration = Duration::from_secs(45);
const STATE_TIMEOUT: Duration = Duration::from_secs(5);
const EOS_BACKOFF_SECONDS: f64 = 120.0;
const POLL_INTERVAL: Duration = Duration::from_millis(100);
const HARDWARE_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const CLOCK_TIME_NONE: u64 = u64::MAX;
const AAC_ENCODER_RATES: [i64; 13] = [
    7350, 8000, 11025, 12000, 16000, 22050, 24000, 32000, 44100, 48000, 64000, 88200, 96000,
];

/// `gstVersionInfo`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Version {
    major: u32,
    minor: u32,
}

impl Version {
    fn at_least(self, major: u32, minor: u32) -> bool {
        if self.major != major {
            self.major > major
        } else {
            self.minor >= minor
        }
    }
}

struct Initialized {
    status: ComponentStatus,
    error: Option<String>,
}

/// The process-wide GStreamer runtime: initialised once, on first use, as
/// `gstInitOnce` does.
#[derive(Default)]
pub struct GstRuntime {
    initialized: OnceLock<Initialized>,
    hardware: OnceLock<Option<(usize, i64, i64)>>,
}

impl GstRuntime {
    pub fn new() -> Self {
        Self::default()
    }

    fn init(&self, config: &Config) -> &Initialized {
        self.initialized.get_or_init(|| {
            // The library is linked, so loading always succeeds; what can
            // fail is initialisation.
            let mut status = ComponentStatus {
                found: env::library_found(config),
                ..ComponentStatus::default()
            };
            status.found = true;
            match gst::init() {
                Ok(()) => {
                    status.available = true;
                    Initialized {
                        status,
                        error: None,
                    }
                }
                Err(error) => {
                    tracing::error!("[GStreamer] error: runtime initialization failed: {error}");
                    Initialized {
                        status,
                        error: Some(error.message().to_string()),
                    }
                }
            }
        })
    }

    fn version(&self) -> Option<Version> {
        let initialized = self.initialized.get()?;
        initialized.error.is_none().then(|| {
            let (major, minor, _, _) = gst::version();
            Version { major, minor }
        })
    }

    /// `effectiveGStreamerVersion`: the runtime's, else the configured one.
    fn effective_version(&self, config: &Config) -> Version {
        if let Some(version) = self.version() {
            return version;
        }
        let value = config.gst_version.max(crate::MIN_GST_VERSION);
        let mut major = value as u32;
        let mut minor = ((value - f64::from(major)) * 100.0).round() as u32;
        if minor >= 100 {
            major += minor / 100;
            minor %= 100;
        }
        Version { major, minor }
    }

    /// `checkGStreamer`.
    pub fn status(&self, config: &Config) -> ComponentStatus {
        let initialized = self.init(config);
        let mut status = initialized.status.clone();
        if let Some(error) = &initialized.error {
            status.error = error.clone();
            return status;
        }
        let (major, minor, micro, nano) = gst::version();
        status.version = if nano != 0 {
            format!("{major}.{minor}.{micro}.{nano}")
        } else {
            format!("{major}.{minor}.{micro}")
        };
        status.works = true;
        status
    }

    /// `CurrentConfig`'s `GSTVersion`: major.minor of the runtime.
    pub fn config_version(&self, config: &Config) -> Option<f64> {
        self.init(config);
        self.version()
            .map(|version| f64::from(version.major) + f64::from(version.minor) / 100.0)
    }

    /// `gstElementAvailable`.
    pub fn element_available(&self, name: &str) -> bool {
        if self.version().is_none() || name.is_empty() {
            return false;
        }
        gst::parse::launch(name).is_ok()
    }

    /// `checkHDRToneMapping`.
    pub fn hdr_tone_mapping(&self, gstreamer: &ComponentStatus) -> ComponentStatus {
        if !gstreamer.works {
            return ComponentStatus::default();
        }
        let found = self.element_available("hdrtonemap");
        ComponentStatus {
            found,
            available: found,
            works: found,
            error: if found {
                String::new()
            } else {
                "hdrtonemap element is not available".into()
            },
            ..ComponentStatus::default()
        }
    }

    /// `hardwareH264Pipeline`: the first hardware encoder that works, probed
    /// once at 4K and then 1080p.
    fn hardware_pipeline(&self, width: i64, height: i64, bitrate: i64, key_int_max: i64) -> String {
        if self.version().is_none() {
            return String::new();
        }
        let selected = *self.hardware.get_or_init(|| {
            for (index, _) in HARDWARE_CANDIDATES.iter().enumerate() {
                for (w, h) in [(3840, 2160), (1920, 1080)] {
                    if probe_hardware(index, w, h) {
                        return Some((index, w, h));
                    }
                }
            }
            None
        });
        let Some((index, max_width, max_height)) = selected else {
            return String::new();
        };
        if width < 128
            || height < 128
            || width & 1 != 0
            || height & 1 != 0
            || width > max_width
            || height > max_height
        {
            return String::new();
        }
        (HARDWARE_CANDIDATES[index].1)(bitrate, key_int_max)
    }
}

type HardwareBuild = fn(i64, i64) -> String;

const HARDWARE_CANDIDATES: [(&str, HardwareBuild); 5] = [
    ("Direct3D11 + Media Foundation", |bitrate, gop| {
        format!(
            "d3d11upload ! d3d11convert ! video/x-raw(memory:D3D11Memory),format=NV12 ! mfh264enc name=video_encoder bitrate={bitrate} gop-size={gop} low-latency=true rc-mode=cbr ! "
        )
    }),
    ("NVIDIA NVENC", |bitrate, gop| {
        format!(
            "videoconvert ! video/x-raw,format=NV12 ! nvh264enc name=video_encoder bitrate={bitrate} gop-size={gop} bframes=0 zerolatency=true rc-mode=cbr ! "
        )
    }),
    ("Intel Quick Sync", |bitrate, gop| {
        format!(
            "videoconvert ! video/x-raw,format=NV12 ! qsvh264enc name=video_encoder bitrate={bitrate} gop-size={gop} b-frames=0 low-latency=true rate-control=cbr ! "
        )
    }),
    ("AMD AMF", |bitrate, gop| {
        format!(
            "videoconvert ! video/x-raw,format=NV12 ! amfh264enc name=video_encoder bitrate={bitrate} gop-size={gop} b-frames=0 usage=low-latency preset=speed rate-control=cbr ! "
        )
    }),
    ("Direct3D12", |bitrate, gop| {
        format!(
            "videoconvert ! video/x-raw,format=NV12 ! d3d12h264enc name=video_encoder bitrate={bitrate} gop-size={gop} rate-control=cbr ! "
        )
    }),
];

fn probe_hardware(index: usize, width: i64, height: i64) -> bool {
    let description = format!(
        "fakesrc num-buffers=2 sizetype=fixed sizemax={} do-timestamp=true format=time ! video/x-raw,format=NV12,width={width},height={height},framerate=30/1 ! {} h264parse ! video/x-h264,profile=main,stream-format=avc,alignment=au ! appsink name=probe_out emit-signals=false sync=false max-buffers=1 drop=false wait-on-eos=false",
        width * height * 3 / 2,
        (HARDWARE_CANDIDATES[index].1)(14_000, 270)
    );
    let Ok(pipeline) = gst::parse::launch(&description) else {
        return false;
    };
    let result = (|| {
        let sink = pipeline
            .downcast_ref::<gst::Bin>()?
            .by_name("probe_out")?
            .downcast::<gst_app::AppSink>()
            .ok()?;
        pipeline.set_state(gst::State::Playing).ok()?;
        sink.try_pull_sample(gst::ClockTime::from_nseconds(
            HARDWARE_PROBE_TIMEOUT.as_nanos() as u64,
        ))
    })();
    let _ = pipeline.set_state(gst::State::Null);
    result.is_some()
}

/// A shared handle so the service can create runners.
pub struct SharedRuntime(pub Arc<GstRuntime>);

impl Runtime for SharedRuntime {
    fn runner(&self, task: Arc<TaskInfo>, audio: i64) -> Result<Box<dyn Runner>, Error> {
        let initialized = self.0.init(&task.config);
        if let Some(error) = &initialized.error {
            return Err(Error::PipelineUnavailable(error.clone()));
        }
        if let Some(video) = task.probe.video()
            && task.config.hdr_to_sdr
            && video.is_hdr_video()
            && !self.0.element_available("hdrtonemap")
        {
            return Err(Error::Other(
                "HDR tone mapping backend is not available".into(),
            ));
        }
        let mut runner = GstRunner {
            runtime: self.0.clone(),
            audio_index: valid_audio_index(&task.probe, audio),
            task,
            state_playing: false,
            ready: Ready::default(),
            position: Arc::new(AtomicU64::new(0f64.to_bits())),
            position_seek_seconds: 0.0,
            reader: None,
            running: None,
            subtitle_stores: None,
            start_probe: None,
            clip_probe: None,
            frozen: false,
        };
        runner.ensure_transient_state();
        Ok(Box::new(runner))
    }

    fn status(&self, config: &Config) -> ComponentStatus {
        self.0.status(config)
    }

    fn config_version(&self, config: &Config) -> Option<f64> {
        self.0.config_version(config)
    }

    fn hdr_tone_mapping(&self, gstreamer: &ComponentStatus) -> ComponentStatus {
        self.0.hdr_tone_mapping(gstreamer)
    }
}

/// `validAudioIndex`: the requested audio track, else the first; -1 without
/// audio.
fn valid_audio_index(probe: &ProbeInfo, requested: i64) -> i64 {
    let mut fallback = -1;
    for track in probe.tracks().iter().filter(|track| track.kind == "audio") {
        if fallback < 0 {
            fallback = track.index;
        }
        if track.index == requested {
            return requested;
        }
    }
    fallback
}

#[derive(Default)]
struct Ready {
    index: i64,
    complete: bool,
    segment: Segment,
}

struct Running {
    pipeline: gst::Element,
    bus: gst::Bus,
    sink: gst_app::AppSink,
    subtitle_sinks: HashMap<i64, gst_app::AppSink>,
}

/// `videoStartProbeState`: the clock time the first video after a seek
/// really starts at.
struct StartProbe {
    requested_ns: u64,
    max_back_diff_ns: u64,
    actual_ns: AtomicU64,
}

impl StartProbe {
    fn accepts(&self, clock: u64) -> bool {
        self.requested_ns <= self.max_back_diff_ns
            || clock >= self.requested_ns - self.max_back_diff_ns
    }
}

struct ProbeHandle {
    pad: gst::Pad,
    id: Mutex<Option<gst::PadProbeId>>,
}

impl ProbeHandle {
    fn remove(&self) {
        if let Some(id) = self
            .id
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        {
            self.pad.remove_probe(id);
        }
    }
}

pub struct GstRunner {
    runtime: Arc<GstRuntime>,
    task: Arc<TaskInfo>,
    audio_index: i64,
    state_playing: bool,
    ready: Ready,
    position: Arc<AtomicU64>,
    position_seek_seconds: f64,
    reader: Option<Reader>,
    running: Option<Running>,
    subtitle_stores: Option<HashMap<i64, Arc<Store>>>,
    start_probe: Option<(Arc<ProbeHandle>, Arc<StartProbe>)>,
    clip_probe: Option<Arc<ProbeHandle>>,
    frozen: bool,
}

impl Runner for GstRunner {
    fn ensure_init(&mut self, cancel: &Cancel, audio: i64, start_index: i64) -> Result<(), Error> {
        let start_index = start_index.max(0);
        self.ensure_transient_state();
        let start_seconds = self.segment_start_seconds(start_index);
        if self.frozen {
            if !self.seek(start_seconds) {
                return Err(Error::SegmentNotReady);
            }
        } else if !self.state_playing {
            self.state_playing = true;
            self.audio_index = valid_audio_index(&self.task.probe, audio);
            self.start_at(start_seconds)?;
        } else if start_index > 0
            && (self.position() - start_seconds).abs() > 0.001
            && !self.seek(start_seconds)
        {
            return Err(Error::SegmentNotReady);
        }
        if self.task.has_init() {
            if self.ready.complete {
                self.complete_ready_segment(start_index);
            }
            return Ok(());
        }
        let deadline = Instant::now() + READ_TIMEOUT;
        while Instant::now() < deadline {
            cancel.check()?;
            let eos = match self.pull_output_sample() {
                Ok(eos) => eos,
                Err(error) => {
                    self.freeze_at_segment(start_index);
                    return Err(error);
                }
            };
            if eos {
                self.freeze_at_segment(start_index);
                return Err(Error::SegmentNotReady);
            }
            if self.task.has_init() {
                if self.ready.complete {
                    self.complete_ready_segment(start_index);
                }
                return Ok(());
            }
        }
        if let Some(error) = self.poll_pipeline_error() {
            self.freeze_at_segment(start_index);
            return Err(error);
        }
        Err(Error::SegmentNotReady)
    }

    fn get_segment(&mut self, cancel: &Cancel, index: i64, audio: i64) -> Result<Segment, Error> {
        self.ensure_transient_state();
        if self.frozen {
            if !self.seek(self.position()) {
                return Err(Error::SegmentNotReady);
            }
        } else if !self.state_playing {
            self.state_playing = true;
            self.audio_index = valid_audio_index(&self.task.probe, audio);
            let start_seconds = self.segment_start_seconds(index);
            self.start_at(start_seconds)?;
        }
        if self.ready.index == index && self.ready.complete {
            return Ok(self.ready.segment.clone());
        }
        self.discard_ready_segment();
        if let Some(cue) = &self.task.cue {
            let Some(segment) = cue.segment(index) else {
                return Err(Error::EndOfStreamExhausted);
            };
            let tolerance = cue.timestamp_scale_ns.max(1);
            if let Err(error) =
                self.reader_mut()
                    .set_target_segment(segment.start_ns, segment.end_ns, tolerance)
            {
                self.freeze_at_segment(index);
                return Err(error);
            }
        }
        let deadline = Instant::now() + READ_TIMEOUT;
        while Instant::now() < deadline {
            cancel.check()?;
            let eos = match self.pull_output_sample() {
                Ok(eos) => eos,
                Err(error) => {
                    self.freeze_at_segment(index);
                    return Err(error);
                }
            };
            if eos {
                return self.finish_end_of_stream(index);
            }
            if self.ready.complete {
                return Ok(self.complete_ready_segment(index));
            }
        }
        if let Some(error) = self.poll_pipeline_error() {
            self.freeze_at_segment(index);
            return Err(error);
        }
        Err(Error::SegmentNotReady)
    }

    /// `Seek`: reuse the running pipeline where possible, else start one at
    /// the position.
    fn seek(&mut self, seconds: f64) -> bool {
        self.ensure_transient_state();
        self.discard_ready_segment();
        self.reset_subtitle_progress(seconds);
        let reuse = self.running.is_some();
        let result = if reuse {
            self.reuse_pipeline(seconds)
        } else {
            self.reader_mut().seek_reset(seconds);
            self.start_pipeline(seconds)
        };
        let actual = match result {
            Ok(actual) => actual,
            Err(error) => {
                tracing::error!(
                    "[GStreamer] error: {} seek requested={seconds:.3}s failed: {error}",
                    self.log_prefix()
                );
                self.freeze_at_position(seconds);
                return false;
            }
        };
        self.reader_mut().seek_reset(actual);
        self.reset_subtitle_progress(actual);
        self.frozen = false;
        self.set_position(actual);
        self.position_seek_seconds = actual;
        self.state_playing = true;
        true
    }

    fn frozen(&mut self) {
        self.freeze_at_position(self.position());
    }

    fn dispose(&mut self) {
        self.stop_pipeline();
        self.discard_ready_segment();
        self.release_transient_state();
        self.state_playing = false;
    }

    fn is_frozen(&self) -> bool {
        self.frozen
    }
}

impl GstRunner {
    fn log_prefix(&self) -> String {
        format!(
            "hash={} file={} audio={}",
            self.task.id, self.task.file_id, self.task.audio
        )
    }

    fn reader_mut(&mut self) -> &mut Reader {
        self.ensure_transient_state();
        self.reader
            .as_mut()
            .expect("the reader exists after ensure_transient_state")
    }

    /// `startPipeline` from `EnsureInit`/`GetSegment`: the reader's timeline
    /// starts at the requested position and moves to where the seek landed.
    fn start_at(&mut self, start_seconds: f64) -> Result<(), Error> {
        if start_seconds > 0.0 {
            self.reader_mut().seek_reset(start_seconds);
            self.position_seek_seconds = start_seconds;
            self.set_position(start_seconds);
        }
        let actual = match self.start_pipeline(start_seconds) {
            Ok(actual) => actual,
            Err(error) => {
                self.freeze_at_position(start_seconds);
                return Err(error);
            }
        };
        if start_seconds > 0.0 {
            self.reader_mut().seek_reset(actual);
            self.position_seek_seconds = actual;
            self.set_position(actual);
        }
        Ok(())
    }

    /// `ensureTransientState`.
    fn ensure_transient_state(&mut self) {
        if self.subtitle_stores.is_none() {
            let mut stores = HashMap::new();
            if self.task.config.subtitles {
                for track in self
                    .task
                    .probe
                    .tracks()
                    .iter()
                    .filter(|track| supported_track(track))
                {
                    stores.insert(track.index, Arc::new(Store::default()));
                }
            }
            self.task.set_subtitle_stores(stores.clone());
            self.subtitle_stores = Some(stores);
        }
        if self.reader.is_none() {
            let diff = if video_is_transcoded(&self.task.config, &self.task.probe) {
                0
            } else {
                self.task.config.segment_diff
            };
            self.reader = Some(Reader::new(
                self.task.config.segment_seconds as f64,
                diff,
                self.task.cue.is_some(),
            ));
        }
    }

    fn release_transient_state(&mut self) {
        self.reader = None;
        self.task.set_subtitle_stores(HashMap::new());
        self.subtitle_stores = None;
    }

    fn set_position(&self, seconds: f64) {
        self.position.store(seconds.to_bits(), Ordering::SeqCst);
    }

    fn position(&self) -> f64 {
        f64::from_bits(self.position.load(Ordering::SeqCst))
    }

    fn segment_start_seconds(&self, index: i64) -> f64 {
        if let Some(segment) = self.task.cue.as_ref().and_then(|cue| cue.segment(index)) {
            return segment.start_ns as f64 / 1_000_000_000.0;
        }
        if index > 0 {
            (index * self.task.config.segment_seconds) as f64
        } else {
            0.0
        }
    }

    /// `createPipelineArgs`.
    fn pipeline_description(&self) -> String {
        let config = self.task.config.clone().normalized();
        let probe = &self.task.probe;
        let version = self.runtime.effective_version(&config);
        let mut description = format!(
            "souphttpsrc location=\"{}\" is-live=false keep-alive=true timeout=60 retries=5 ",
            self.task.source_url
        );
        if version.at_least(1, 26) {
            description.push_str("retry-backoff-factor=0.5 retry-backoff-max=10 ");
        }
        if probe.is_avi_container() {
            description.push_str(" ! avidemux name=d ");
        } else {
            description.push_str(" ! matroskademux name=d ");
        }
        description.push_str(
            "multiqueue name=mq use-buffering=false max-size-buffers=5 max-size-bytes=0 max-size-time=0 ",
        );
        description.push_str("d.video_0 ! mq.sink_0 ");
        if video_is_transcoded(&config, probe) {
            self.transcode_to_h264(&mut description);
        } else if probe.is_h264() {
            description.push_str("mq.src_0 ! h264parse config-interval=0 ! h264timestamper name=video_timestamper ! video/x-h264,stream-format=avc,alignment=au ! mux.video_0 ");
        } else if probe.is_h265() {
            description.push_str("mq.src_0 ! h265parse config-interval=0 ! h265timestamper name=video_timestamper ! video/x-h265,stream-format=hvc1,alignment=au ! mux.video_0 ");
        } else if probe.is_av1() {
            description.push_str(
                "mq.src_0 ! av1parse ! video/x-av1,stream-format=obu-stream,alignment=tu ! mux.video_0 ",
            );
        } else if probe.is_vp9() {
            description
                .push_str("mq.src_0 ! vp9parse ! video/x-vp9,alignment=frame ! mux.video_0 ");
        }
        if let Some(track) = probe.audio_track(self.audio_index) {
            description.push_str(&format!("d.audio_{} ! mq.sink_1 mq.src_1 ! ", track.index));
            if track.is_aac_audio() {
                description.push_str(
                    "aacparse ! audio/mpeg,mpegversion=4,stream-format=raw ! mux.audio_0 ",
                );
            } else {
                self.encode_aac(&mut description, &config, track);
            }
        }
        if config.subtitles {
            for track in probe.tracks().iter().filter(|track| supported_track(track)) {
                description.push_str(&format!(
                    "d.{} ! queue max-size-buffers=16 max-size-bytes=0 max-size-time=0 ! ",
                    track.pad_name
                ));
                if track.codec == "ass" || track.codec == "ssa" {
                    description.push_str("ssaparse ! ");
                }
                description.push_str(&format!(
                    "webvttenc ! appsink name=subs_{} emit-signals=false sync=false async=false max-buffers=16",
                    track.index
                ));
                description.push_str(if version.at_least(1, 28) {
                    " leaky-type=none"
                } else {
                    " drop=false"
                });
                description.push_str(" wait-on-eos=false ");
            }
        }
        description.push_str("mp4mux name=mux fragment-mode=dash-or-mss fragment-duration=");
        if self.task.cue.is_some() {
            description.push('1');
        } else {
            description.push_str(&(config.segment_seconds * 1000).to_string());
        }
        description.push_str(
            " streamable=true ! appsink name=out emit-signals=false sync=false max-buffers=1",
        );
        description.push_str(if version.at_least(1, 28) {
            " leaky-type=none"
        } else {
            " drop=false"
        });
        description.push_str(" wait-on-eos=false");
        description
    }

    fn encode_aac(&self, description: &mut String, config: &Config, track: &TrackInfo) {
        let channels = aac_channels(config, Some(track.channels));
        let rate = aac_sample_rate(config, track);
        let mut bitrate = config.aac_bitrate_kbps * 1000;
        if channels > 2 {
            bitrate *= 2;
        }
        description.push_str(&format!(
            "decodebin ! audioconvert dithering=none noise-shaping=none ! audioresample quality=2 sinc-filter-mode=full ! audio/x-raw,format=F32LE,layout=interleaved,rate={rate},channels={channels}"
        ));
        if channels == 6 {
            // Normalize 5.1(side) decoders to the browser-compatible AAC 5.1 layout.
            description.push_str(",channel-mask=(bitmask)0x000000000000003f");
        }
        description.push_str(&format!(
            " ! avenc_aac bitrate={bitrate} ! aacparse ! audio/mpeg,mpegversion=4,stream-format=raw,rate={rate},channels={channels} ! mux.audio_0 "
        ));
    }

    /// `transcodeToH264`.
    fn transcode_to_h264(&self, description: &mut String) {
        let config = &self.task.config;
        let video = self.task.probe.video();
        let tone_map = config.hdr_to_sdr && video.is_some_and(TrackInfo::is_hdr_video);
        let (num, den) = video.map_or((0, 0), |video| (video.frame_rate_num, video.frame_rate_den));
        let mut key_int_max = 25 * config.segment_seconds;
        if num > 0 && den > 0 {
            key_int_max = ((num * config.segment_seconds) as f64 / den as f64).round() as i64;
            key_int_max = key_int_max.max(1);
        }
        description.push_str("mq.src_0 ! decodebin ! ");
        if tone_map {
            let transfer = if video.is_some_and(|video| video.video_transfer == "hlg") {
                "hlg"
            } else {
                "pq"
            };
            description.push_str(&format!(
                "hdrtonemap transfer={transfer} use-opencl={} ! ",
                config.use_gpu
            ));
        }
        let mut encoder = String::new();
        if config.use_gpu
            && config.hardware_acceleration
            && let Some(video) = video
        {
            encoder = self.runtime.hardware_pipeline(
                video.width,
                video.height,
                config.video_bitrate,
                key_int_max,
            );
        }
        if encoder.is_empty() {
            let preset = if config.x264_ultrafast {
                "ultrafast"
            } else {
                "veryfast"
            };
            if !tone_map {
                description.push_str("videoconvert ! ");
            }
            description.push_str(&format!(
                "video/x-raw,format=I420 ! x264enc name=video_encoder tune=zerolatency speed-preset={preset} bitrate={} key-int-max={key_int_max} bframes=0 byte-stream=false ! video/x-h264,profile=main,stream-format=avc,alignment=au ! ",
                config.video_bitrate
            ));
        } else {
            description.push_str(&encoder);
        }
        description.push_str("h264parse config-interval=0 ! h264timestamper name=video_timestamper ! video/x-h264,profile=main,stream-format=avc,alignment=au ! mux.video_0 ");
    }

    /// `startPipeline`.
    fn start_pipeline(&mut self, seconds: f64) -> Result<f64, Error> {
        self.ensure_transient_state();
        let pipeline = gst::parse::launch(&self.pipeline_description())
            .map_err(|error| Error::Other(error.message().to_string()))?;
        let bin = pipeline
            .clone()
            .downcast::<gst::Bin>()
            .map_err(|_| Error::Other("appsink element is not available".into()))?;
        let Some(sink) = bin
            .by_name("out")
            .and_then(|sink| sink.downcast::<gst_app::AppSink>().ok())
        else {
            let _ = pipeline.set_state(gst::State::Null);
            return Err(Error::Other("appsink element is not available".into()));
        };
        let Some(bus) = pipeline.bus() else {
            let _ = pipeline.set_state(gst::State::Null);
            return Err(Error::Other("gstreamer bus is not available".into()));
        };
        let mut subtitle_sinks = HashMap::new();
        for index in self.subtitle_stores.iter().flat_map(HashMap::keys) {
            if let Some(sink) = bin
                .by_name(&format!("subs_{index}"))
                .and_then(|sink| sink.downcast::<gst_app::AppSink>().ok())
            {
                subtitle_sinks.insert(*index, sink);
            }
        }
        let cleanup = |runner: &mut Self| {
            runner.remove_seek_probes();
            let _ = pipeline.set_state(gst::State::Null);
        };
        let mut actual = seconds;
        if seconds > 0.0 {
            if let Err(error) = set_pipeline_state(&pipeline, &bus, gst::State::Paused) {
                cleanup(self);
                return Err(error);
            }
            let accurate = self.task.cue.is_some();
            if let Some(error) = pop_bus_error(&bus, Duration::ZERO) {
                cleanup(self);
                return Err(error);
            }
            drain_bus(&bus, &[gst::MessageType::AsyncDone, gst::MessageType::Eos]);
            let seek_ns = (seconds * 1_000_000_000.0).round();
            if seek_ns < 0.0 {
                cleanup(self);
                return Err(Error::Other("gstreamer seek position is negative".into()));
            }
            let seek_ns = seek_ns as u64;
            self.install_seek_probes(&bin, seek_ns, accurate);
            if let Err(error) = send_video_seek(&bin, accurate, seek_ns) {
                cleanup(self);
                return Err(Error::Other(format!("gstreamer seek failed: {error}")));
            }
            if let Err(error) = wait_for_seek_done(&bus, STATE_TIMEOUT) {
                cleanup(self);
                return Err(Error::Other(format!(
                    "gstreamer seek did not finish: {error}"
                )));
            }
            match pipeline.state(clock(STATE_TIMEOUT)).0 {
                Ok(gst::StateChangeSuccess::Success | gst::StateChangeSuccess::NoPreroll) => {}
                Ok(gst::StateChangeSuccess::Async) => {
                    let error = pop_bus_error(&bus, Duration::ZERO).unwrap_or_else(|| {
                        Error::Other(format!("gstreamer seek to {seconds:.3}s timed out"))
                    });
                    cleanup(self);
                    return Err(error);
                }
                Err(_) => {
                    let error = pop_bus_error(&bus, Duration::ZERO).unwrap_or_else(|| {
                        Error::Other(format!("gstreamer seek to {seconds:.3}s failed"))
                    });
                    cleanup(self);
                    return Err(error);
                }
            }
            actual = query_position(&pipeline).unwrap_or(seconds);
        }
        if let Err(error) = set_pipeline_state(&pipeline, &bus, gst::State::Playing) {
            cleanup(self);
            return Err(error);
        }
        self.running = Some(Running {
            pipeline,
            bus,
            sink,
            subtitle_sinks,
        });
        self.reset_subtitle_progress(actual);
        Ok(actual)
    }

    /// `reusePipeline`: flush the running pipeline to a new position; the
    /// muxer, timestamper and encoder are reset so the new run starts clean.
    fn reuse_pipeline(&mut self, seconds: f64) -> Result<f64, Error> {
        let accurate = self.task.cue.is_some();
        let transcoded = video_is_transcoded(&self.task.config, &self.task.probe);
        let Some(running) = &self.running else {
            return Err(Error::Other("pipeline cannot be reused".into()));
        };
        let (pipeline, bus) = (running.pipeline.clone(), running.bus.clone());
        let sink = running.sink.clone().upcast::<gst::Element>();
        set_pipeline_state(&pipeline, &bus, gst::State::Paused)
            .map_err(|error| Error::Other(format!("pause pipeline before seek: {error}")))?;
        let bin = pipeline
            .clone()
            .downcast::<gst::Bin>()
            .map_err(|_| Error::Other("mp4 mux is not available for seek reset".into()))?;
        let Some(mux) = bin.by_name("mux") else {
            return Err(Error::Other(
                "mp4 mux is not available for seek reset".into(),
            ));
        };
        let timestamper = bin.by_name("video_timestamper");
        let encoder = if transcoded {
            let Some(encoder) = bin.by_name("video_encoder") else {
                return Err(Error::Other(
                    "video encoder is not available for seek reset".into(),
                ));
            };
            Some(encoder)
        } else {
            None
        };
        let order = [Some(sink), Some(mux), timestamper, encoder];
        for element in order.iter().flatten() {
            if element.set_state(gst::State::Ready).is_err() {
                return Err(Error::Other("reset pipeline child before seek".into()));
            }
        }
        for element in order.iter().rev().flatten() {
            if element.set_state(gst::State::Paused).is_err() {
                return Err(Error::Other("pause pipeline child after seek reset".into()));
            }
        }
        self.reader_mut().seek_reset(seconds);
        self.position_seek_seconds = seconds;
        self.set_position(seconds);
        let seek_ns = (seconds * 1_000_000_000.0).round();
        if seek_ns < 0.0 {
            return Err(Error::Other("gstreamer seek position is negative".into()));
        }
        let seek_ns = seek_ns as u64;
        self.install_seek_probes(&bin, seek_ns, accurate);
        if let Some(error) = pop_bus_error(&bus, Duration::ZERO) {
            return Err(error);
        }
        drain_bus(&bus, &[gst::MessageType::AsyncDone, gst::MessageType::Eos]);
        send_video_seek(&bin, accurate, seek_ns).map_err(|error| {
            Error::Other(format!(
                "gstreamer seek failed while reusing pipeline: {error}"
            ))
        })?;
        wait_for_seek_done(&bus, STATE_TIMEOUT).map_err(|error| {
            Error::Other(format!(
                "gstreamer seek did not finish while reusing pipeline: {error}"
            ))
        })?;
        match pipeline.state(clock(STATE_TIMEOUT)).0 {
            Ok(gst::StateChangeSuccess::Success | gst::StateChangeSuccess::NoPreroll) => {}
            other => {
                if let Some(error) = pop_bus_error(&bus, Duration::ZERO) {
                    return Err(error);
                }
                let code = match other {
                    Ok(gst::StateChangeSuccess::Async) => 2,
                    _ => 0,
                };
                return Err(Error::Other(format!(
                    "gstreamer seek state={code} while reusing pipeline"
                )));
            }
        }
        let actual = query_position(&pipeline).unwrap_or(seconds);
        set_pipeline_state(&pipeline, &bus, gst::State::Playing)
            .map_err(|error| Error::Other(format!("resume pipeline after seek: {error}")))?;
        Ok(actual)
    }

    /// `installVideoSeekProbes`.
    fn install_seek_probes(&mut self, bin: &gst::Bin, requested_ns: u64, accurate: bool) {
        self.remove_seek_probes();
        self.install_start_probe(bin, requested_ns);
        let clip_start = if accurate {
            requested_ns
        } else {
            CLOCK_TIME_NONE
        };
        self.install_clip_probe(bin, clip_start);
    }

    fn install_start_probe(&mut self, bin: &gst::Bin, requested_ns: u64) {
        let Some(pad) = element_pad(bin, "mq", "src_0") else {
            return;
        };
        let max_back_diff_ns = match &self.task.cue {
            Some(cue) => cue.max_duration_ns,
            None => self.task.config.segment_seconds.max(1) as u64 * 1_000_000_000,
        };
        let state = Arc::new(StartProbe {
            requested_ns,
            max_back_diff_ns,
            actual_ns: AtomicU64::new(CLOCK_TIME_NONE),
        });
        let handle = Arc::new(ProbeHandle {
            pad: pad.clone(),
            id: Mutex::new(None),
        });
        let probe_state = state.clone();
        let probe_handle = Arc::downgrade(&handle);
        let id = pad.add_probe(
            gst::PadProbeType::EVENT_DOWNSTREAM | gst::PadProbeType::BUFFER,
            move |_, info| match &info.data {
                Some(gst::PadProbeData::Event(event)) => {
                    if let Some(clock) = segment_clock_time(event)
                        && probe_state.accepts(clock)
                    {
                        probe_state.actual_ns.store(clock, Ordering::SeqCst);
                    }
                    gst::PadProbeReturn::Ok
                }
                Some(gst::PadProbeData::Buffer(buffer)) => {
                    let clock = buffer.pts().or(buffer.dts()).map(gst::ClockTime::nseconds);
                    match clock {
                        Some(clock) if probe_state.accepts(clock) => {
                            probe_state.actual_ns.store(clock, Ordering::SeqCst);
                            if let Some(handle) = probe_handle.upgrade() {
                                handle
                                    .id
                                    .lock()
                                    .unwrap_or_else(|error| error.into_inner())
                                    .take();
                            }
                            gst::PadProbeReturn::Remove
                        }
                        _ => gst::PadProbeReturn::Ok,
                    }
                }
                _ => gst::PadProbeReturn::Ok,
            },
        );
        *handle.id.lock().unwrap_or_else(|error| error.into_inner()) = id;
        self.start_probe = Some((handle, state));
    }

    /// `installVideoSegmentClipProbe`: after an accurate seek, remuxed H.264
    /// and H.265 drop frames before the requested start.
    fn install_clip_probe(&mut self, bin: &gst::Bin, requested_start: u64) {
        let passthrough = !video_is_transcoded(&self.task.config, &self.task.probe);
        if !passthrough || (!self.task.probe.is_h264() && !self.task.probe.is_h265()) {
            return;
        }
        let Some(pad) = element_pad(bin, "video_timestamper", "src") else {
            return;
        };
        let segment_start = Arc::new(AtomicU64::new(requested_start));
        let id = pad.add_probe(
            gst::PadProbeType::EVENT_DOWNSTREAM | gst::PadProbeType::BUFFER,
            move |_, info| match &info.data {
                Some(gst::PadProbeData::Event(event)) => {
                    match event.view() {
                        gst::EventView::FlushStart(_) => {
                            segment_start.store(requested_start, Ordering::SeqCst);
                        }
                        gst::EventView::Segment(segment) => {
                            if let Some(segment) =
                                segment.segment().downcast_ref::<gst::ClockTime>()
                            {
                                let mut start = segment
                                    .start()
                                    .map_or(CLOCK_TIME_NONE, gst::ClockTime::nseconds);
                                if requested_start != CLOCK_TIME_NONE && start < requested_start {
                                    start = requested_start;
                                }
                                segment_start.store(start, Ordering::SeqCst);
                            }
                        }
                        _ => {}
                    }
                    gst::PadProbeReturn::Ok
                }
                Some(gst::PadProbeData::Buffer(buffer)) => {
                    let start = segment_start.load(Ordering::SeqCst);
                    match buffer.pts() {
                        Some(pts) if start != CLOCK_TIME_NONE && pts.nseconds() < start => {
                            gst::PadProbeReturn::Drop
                        }
                        _ => gst::PadProbeReturn::Ok,
                    }
                }
                _ => gst::PadProbeReturn::Ok,
            },
        );
        self.clip_probe = Some(Arc::new(ProbeHandle {
            pad,
            id: Mutex::new(id),
        }));
    }

    fn remove_seek_probes(&mut self) {
        if let Some((handle, _)) = self.start_probe.take() {
            handle.remove();
        }
        if let Some(handle) = self.clip_probe.take() {
            handle.remove();
        }
    }

    /// `applyPendingVideoStart`: once the first video after a seek has been
    /// seen, the reader's timeline starts at its clock time.
    fn apply_pending_video_start(&mut self) {
        let Some((_, state)) = &self.start_probe else {
            return;
        };
        let clock = state.actual_ns.swap(CLOCK_TIME_NONE, Ordering::SeqCst);
        if clock == CLOCK_TIME_NONE {
            return;
        }
        let seconds = clock as f64 / 1_000_000_000.0;
        if let Some(reader) = self.reader.as_mut() {
            reader.set_timeline_offset_ns(clock);
        }
        self.position_seek_seconds = seconds;
        self.set_position(seconds);
    }

    /// `pullOutputSample`: one appsink sample into the reader; `true` at EOS.
    fn pull_output_sample(&mut self) -> Result<bool, Error> {
        if self.running.is_none() || self.reader.is_none() {
            return Err(Error::Other("gstreamer output is not available".into()));
        }
        if let Some(error) = self.poll_pipeline_error() {
            return Err(error);
        }
        self.drain_subtitles();
        let running = self.running.as_ref().expect("checked above");
        let sample = running.sink.try_pull_sample(clock(POLL_INTERVAL));
        let Some(sample) = sample else {
            if let Some(error) = self.poll_pipeline_error() {
                return Err(error);
            }
            let running = self.running.as_ref().expect("checked above");
            if !running.sink.is_eos() {
                return Ok(false);
            }
            if let Some(error) = self.early_end_of_stream_error() {
                return Err(error);
            }
            return Ok(true);
        };
        self.apply_pending_video_start();
        if let Some(buffer) = sample.buffer() {
            let size = buffer.size() as u64;
            if size > crate::mp4box::MAX_SAMPLE_BYTES {
                return Err(Error::Other(format!(
                    "mp4 parser: gst buffer exceeds safety limit: {size} bytes"
                )));
            }
            let map = buffer
                .map_readable()
                .map_err(|_| Error::Other("mp4 parser: gst_buffer_map failed".into()))?;
            let result = self.reader_mut().push(map.as_slice());
            drop(map);
            result.map_err(|error| Error::Other(format!("mp4 parser: {error}")))?;
            self.collect_reader_output();
        }
        if let Some(error) = self.poll_pipeline_error() {
            return Err(error);
        }
        self.drain_subtitles();
        Ok(false)
    }

    /// The reader's callbacks: `setInitMP4` and `acceptSegment`.
    fn collect_reader_output(&mut self) {
        let Some(reader) = self.reader.as_mut() else {
            return;
        };
        if let Some(init) = reader.take_init() {
            self.task.set_init(init);
        }
        if let Some(segment) = reader.take_segment() {
            self.set_position(if segment.end_seconds >= segment.start_seconds {
                segment.end_seconds
            } else {
                segment.start_seconds
            });
            self.ready.segment = segment;
            self.ready.complete = true;
        }
    }

    /// `pollPipelineError`: the first bus error; other messages are dropped
    /// so the bus does not grow, as the reference's bus watch drains it.
    fn poll_pipeline_error(&self) -> Option<Error> {
        let running = self.running.as_ref()?;
        while let Some(message) = running.bus.pop() {
            if let gst::MessageView::Error(error) = message.view() {
                return Some(message_error(error));
            }
        }
        None
    }

    /// `earlyEndOfStreamError`: EOS well before the probed duration.
    fn early_end_of_stream_error(&self) -> Option<Error> {
        if self.task.probe.duration_ns <= 0 {
            return None;
        }
        let duration = self.task.probe.duration_ns as f64 / 1_000_000_000.0;
        let threshold = (duration - EOS_BACKOFF_SECONDS).max(0.0);
        let position = self.position();
        if position.is_finite() && position >= threshold {
            return None;
        }
        Some(Error::EarlyEndOfStream(format!(
            ": position={position:.3}s threshold={threshold:.3}s duration={duration:.3}s"
        )))
    }

    fn drain_subtitles(&mut self) {
        if !self.task.config.subtitles {
            return;
        }
        let Some(running) = &self.running else {
            return;
        };
        let max_back = self.max_segment_duration_ns();
        for (index, sink) in &running.subtitle_sinks {
            let Some(store) = self
                .subtitle_stores
                .as_ref()
                .and_then(|stores| stores.get(index))
            else {
                continue;
            };
            while let Some(sample) = sink.try_pull_sample(gst::ClockTime::ZERO) {
                let Some(buffer) = sample.buffer() else {
                    continue;
                };
                let Ok(map) = buffer.map_readable() else {
                    continue;
                };
                let chunk = String::from_utf8_lossy(map.as_slice()).into_owned();
                store.append_vtt(&chunk, self.position_seek_seconds, max_back);
            }
        }
    }

    fn max_segment_duration_ns(&self) -> u64 {
        match &self.task.cue {
            Some(cue) if cue.max_duration_ns > 0 => cue.max_duration_ns,
            _ => self.task.config.segment_seconds.max(1) as u64 * 1_000_000_000,
        }
    }

    fn reset_subtitle_progress(&self, seconds: f64) {
        let value = progress_ns(seconds);
        for store in self.subtitle_stores.iter().flat_map(HashMap::values) {
            store.set_video_read_to(value);
        }
    }

    /// `drainEndOfStream` / `finishEndOfStream`.
    fn finish_end_of_stream(&mut self, index: i64) -> Result<Segment, Error> {
        let result = self.drain_end_of_stream(index);
        if let Err(error) = &result
            && *error != Error::EndOfStreamExhausted
        {
            tracing::error!(
                "[GStreamer] error: {} mp4 EOS drain failed for segment={index}: {error}",
                self.log_prefix()
            );
            self.freeze_at_segment(index);
        }
        result
    }

    fn drain_end_of_stream(&mut self, index: i64) -> Result<Segment, Error> {
        let Some(reader) = self.reader.as_mut() else {
            return Err(Error::SegmentNotReady);
        };
        let completed = match reader.try_process_deferred() {
            Ok(completed) => completed,
            Err(error) => {
                if reader.has_video() && !reader.video_starts_with_sync() {
                    return Err(reader.undecodable_remainder_error());
                }
                return Err(error);
            }
        };
        self.collect_reader_output();
        if completed {
            if !self.ready.complete {
                return Err(Error::Other(
                    "mp4 reader completed a segment without onSegment callback".into(),
                ));
            }
            return Ok(self.complete_ready_segment(index));
        }
        let reader = self.reader.as_mut().expect("present above");
        let completed = reader.try_build_end_of_stream_remainder()?;
        self.collect_reader_output();
        if completed {
            if !self.ready.complete {
                return Err(Error::Other(
                    "mp4 reader completed EOS remainder without onSegment callback".into(),
                ));
            }
            return Ok(self.complete_ready_segment(index));
        }
        if let Some(error) = self.reader.as_ref().and_then(Reader::end_of_stream_error) {
            return Err(error);
        }
        Err(Error::EndOfStreamExhausted)
    }

    /// `completeReadySegment`.
    fn complete_ready_segment(&mut self, index: i64) -> Segment {
        self.ready.index = index.max(0);
        let (_, mut read_to) = self.task.subtitle_range(index);
        read_to = read_to.max(self.ready.segment.end_ns);
        for store in self.subtitle_stores.iter().flat_map(HashMap::values) {
            store.advance_video_read_to(read_to);
        }
        self.ready.segment.clone()
    }

    fn discard_ready_segment(&mut self) {
        self.ready = Ready {
            index: -1,
            ..Ready::default()
        };
    }

    fn freeze_at_segment(&mut self, index: i64) {
        let seconds = if index >= 0 {
            self.segment_start_seconds(index)
        } else {
            self.position()
        };
        self.freeze_at_position(seconds);
    }

    /// `freezeAtPosition`: resources released until the next request thaws
    /// the task at this position.
    fn freeze_at_position(&mut self, seconds: f64) {
        self.stop_pipeline();
        self.discard_ready_segment();
        self.release_transient_state();
        self.task.clear_init();
        self.frozen = true;
        self.set_position(seconds);
        self.position_seek_seconds = seconds;
        self.state_playing = false;
    }

    fn stop_pipeline(&mut self) {
        self.remove_seek_probes();
        if let Some(running) = self.running.take() {
            let _ = running.pipeline.set_state(gst::State::Null);
        }
    }
}

impl Drop for GstRunner {
    fn drop(&mut self) {
        self.stop_pipeline();
    }
}

fn aac_sample_rate(config: &Config, track: &TrackInfo) -> i64 {
    let mut rate = config.aac_samplerate;
    if rate <= 0 {
        rate = track.rate;
    }
    if rate <= 0 {
        rate = DEFAULT_AAC_SAMPLE_RATE;
    }
    let mut best = AAC_ENCODER_RATES[0];
    for candidate in &AAC_ENCODER_RATES[1..] {
        if (rate - candidate).abs() < (rate - best).abs() {
            best = *candidate;
        }
    }
    best
}

fn progress_ns(seconds: f64) -> u64 {
    if seconds <= 0.0 || seconds.is_nan() {
        return 0;
    }
    if seconds.is_infinite() || seconds >= u64::MAX as f64 / 1_000_000_000.0 {
        return u64::MAX;
    }
    (seconds * 1_000_000_000.0).round() as u64
}

fn clock(duration: Duration) -> gst::ClockTime {
    gst::ClockTime::from_nseconds(duration.as_nanos() as u64)
}

fn element_pad(bin: &gst::Bin, element: &str, pad: &str) -> Option<gst::Pad> {
    bin.by_name(element)?.static_pad(pad)
}

/// `gstSegmentClockTime`: a TIME segment's `time`, else its `start`.
fn segment_clock_time(event: &gst::Event) -> Option<u64> {
    let gst::EventView::Segment(segment) = event.view() else {
        return None;
    };
    let segment = segment.segment().downcast_ref::<gst::ClockTime>()?;
    segment
        .time()
        .or(segment.start())
        .map(gst::ClockTime::nseconds)
}

/// `sendVideoSeekEvent`: the seek goes upstream from the video queue, so
/// only the video branch decides where the demuxer lands.
fn send_video_seek(bin: &gst::Bin, accurate: bool, position_ns: u64) -> Result<(), &'static str> {
    let Some(multiqueue) = bin.by_name("mq") else {
        return Err("multiqueue is not available");
    };
    let Some(pad) = multiqueue.static_pad("src_0") else {
        return Err("multiqueue video pad is not available");
    };
    let mut flags = gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT | gst::SeekFlags::SNAP_AFTER;
    if accurate {
        flags |= gst::SeekFlags::ACCURATE;
    }
    let event = gst::event::Seek::new(
        1.0,
        flags,
        gst::SeekType::Set,
        gst::ClockTime::from_nseconds(position_ns),
        gst::SeekType::None,
        gst::ClockTime::NONE,
    );
    if pad.send_event(event) {
        Ok(())
    } else {
        Err("video seek event returned false")
    }
}

/// `setPipelineState`.
fn set_pipeline_state(
    pipeline: &gst::Element,
    bus: &gst::Bus,
    state: gst::State,
) -> Result<(), Error> {
    let code = state as i32;
    if pipeline.set_state(state).is_err() {
        return Err(pop_bus_error(bus, Duration::ZERO).unwrap_or_else(|| {
            Error::Other(format!(
                "gstreamer failed to request state change to {code}"
            ))
        }));
    }
    match pipeline.state(clock(STATE_TIMEOUT)).0 {
        Ok(gst::StateChangeSuccess::Success | gst::StateChangeSuccess::NoPreroll) => Ok(()),
        Ok(gst::StateChangeSuccess::Async) => Err(pop_bus_error(bus, Duration::ZERO)
            .unwrap_or_else(|| {
                Error::Other(format!("gstreamer state change to {code} timed out"))
            })),
        Err(_) => Err(pop_bus_error(bus, Duration::ZERO)
            .unwrap_or_else(|| Error::Other(format!("gstreamer state change to {code} failed")))),
    }
}

fn query_position(pipeline: &gst::Element) -> Option<f64> {
    pipeline
        .query_position::<gst::ClockTime>()
        .map(|position| position.nseconds() as f64 / 1_000_000_000.0)
}

/// `parseMessageError`: the GError message, then `: debug`.
fn message_error(error: &gst::message::Error) -> Error {
    let mut message = error.error().message().to_string();
    if let Some(debug) = error.debug().filter(|debug| !debug.is_empty()) {
        if message.is_empty() {
            message = debug.to_string();
        } else {
            message = format!("{message}: {debug}");
        }
    }
    if message.is_empty() {
        message = "gstreamer bus error".into();
    }
    Error::Other(message)
}

fn pop_bus_error(bus: &gst::Bus, timeout: Duration) -> Option<Error> {
    let message = bus.timed_pop_filtered(clock(timeout), &[gst::MessageType::Error])?;
    match message.view() {
        gst::MessageView::Error(error) => Some(message_error(error)),
        _ => None,
    }
}

fn drain_bus(bus: &gst::Bus, types: &[gst::MessageType]) {
    while bus
        .timed_pop_filtered(gst::ClockTime::ZERO, types)
        .is_some()
    {}
}

/// `waitForSeekDone`: `ASYNC_DONE` within the timeout, failing on an error
/// or EOS; not seeing it is not an error by itself.
fn wait_for_seek_done(bus: &gst::Bus, timeout: Duration) -> Result<bool, Error> {
    let eos = || Error::Other("gstreamer reached EOS while completing seek".into());
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(error) = pop_bus_error(bus, Duration::ZERO) {
            return Err(error);
        }
        if bus
            .timed_pop_filtered(gst::ClockTime::ZERO, &[gst::MessageType::Eos])
            .is_some()
        {
            return Err(eos());
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        let wait = remaining.min(POLL_INTERVAL);
        if bus
            .timed_pop_filtered(clock(wait), &[gst::MessageType::AsyncDone])
            .is_some()
        {
            return Ok(true);
        }
    }
    if let Some(error) = pop_bus_error(bus, Duration::ZERO) {
        return Err(error);
    }
    if bus
        .timed_pop_filtered(gst::ClockTime::ZERO, &[gst::MessageType::Eos])
        .is_some()
    {
        return Err(eos());
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_runtime_reports_itself_like_the_reference() {
        let runtime = GstRuntime::new();
        let status = runtime.status(&Config::platform_defaults());
        assert!(
            status.found && status.available && status.works,
            "{status:?}"
        );
        assert!(status.version.starts_with("1."), "{status:?}");
        assert!(
            runtime
                .config_version(&Config::platform_defaults())
                .unwrap()
                >= 1.22
        );
        // Debian's GStreamer has no hdrtonemap element.
        assert_eq!(
            runtime.hdr_tone_mapping(&status).error,
            "hdrtonemap element is not available"
        );
        assert_eq!(
            runtime.hdr_tone_mapping(&ComponentStatus::default()),
            ComponentStatus::default()
        );
    }

    #[test]
    fn aac_rates_snap_to_what_the_encoder_accepts() {
        let track = |rate| TrackInfo {
            kind: "audio".into(),
            rate,
            ..TrackInfo::default()
        };
        let config = Config::platform_defaults();
        assert_eq!(aac_sample_rate(&config, &track(8000)), 8000);
        assert_eq!(aac_sample_rate(&config, &track(44_000)), 44_100);
        assert_eq!(aac_sample_rate(&config, &track(0)), 48_000);
        assert_eq!(valid_audio_index(&ProbeInfo::default(), 0), -1);
    }
}
