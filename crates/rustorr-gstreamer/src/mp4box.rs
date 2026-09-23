//! `server/gstreamer/mp4box.go`: turns mp4mux's fragmented output into HLS
//! segments. mp4mux writes one `moof`+`mdat` per track fragment; the reader
//! queues them per track and, once enough video is buffered, merges video
//! fragments up to a key frame and the audio covering the same time into one
//! `moof` with a canonical `tfhd`/`tfdt`/`trun` per track. The output is
//! byte-for-byte what the reference produces for the same mp4mux stream.

use crate::Error;

const fn four_cc(code: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*code)
}

const STYP: u32 = four_cc(b"styp");
const SIDX: u32 = four_cc(b"sidx");
const EMSG: u32 = four_cc(b"emsg");
const FREE: u32 = four_cc(b"free");
const PRFT: u32 = four_cc(b"prft");
const MOOV: u32 = four_cc(b"moov");
const MOOF: u32 = four_cc(b"moof");
const MDAT: u32 = four_cc(b"mdat");
const MFHD: u32 = four_cc(b"mfhd");
const TRAF: u32 = four_cc(b"traf");
const TFHD: u32 = four_cc(b"tfhd");
const TFDT: u32 = four_cc(b"tfdt");
const TRUN: u32 = four_cc(b"trun");
const TRAK: u32 = four_cc(b"trak");
const TKHD: u32 = four_cc(b"tkhd");
const MDIA: u32 = four_cc(b"mdia");
const MDHD: u32 = four_cc(b"mdhd");
const HDLR: u32 = four_cc(b"hdlr");
const MVEX: u32 = four_cc(b"mvex");
const TREX: u32 = four_cc(b"trex");
const MFRA: u32 = four_cc(b"mfra");

const HANDLER_VIDEO: u32 = four_cc(b"vide");
const HANDLER_AUDIO: u32 = four_cc(b"soun");

const TFHD_BASE_DATA_OFFSET: u32 = 0x00_0001;
const TFHD_SAMPLE_DESCRIPTION_INDEX: u32 = 0x00_0002;
const TFHD_DEFAULT_DURATION: u32 = 0x00_0008;
const TFHD_DEFAULT_SIZE: u32 = 0x00_0010;
const TFHD_DEFAULT_FLAGS: u32 = 0x00_0020;
const TFHD_DURATION_IS_EMPTY: u32 = 0x01_0000;
const TFHD_DEFAULT_BASE_IS_MOOF: u32 = 0x02_0000;
const TRUN_DATA_OFFSET: u32 = 0x00_0001;
const TRUN_FIRST_SAMPLE_FLAGS: u32 = 0x00_0004;
const TRUN_DURATION: u32 = 0x00_0100;
const TRUN_SIZE: u32 = 0x00_0200;
const TRUN_FLAGS: u32 = 0x00_0400;
const TRUN_COMPOSITION_OFFSET: u32 = 0x00_0800;
const MAX_TRUN_SAMPLES: u32 = 1_000_000;
const CANONICAL_TFHD_SIZE: usize = 20;
/// gst_api.go: the largest sample the reference maps.
pub const MAX_SAMPLE_BYTES: u64 = 256 * 1024 * 1024;

fn other(message: impl Into<String>) -> Error {
    Error::Other(message.into())
}

/// One HLS media segment: `moof`+`mdat` bytes and its place on the timeline.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Segment {
    pub data: Vec<u8>,
    pub start_ns: u64,
    pub end_ns: u64,
    pub start_seconds: f64,
    pub end_seconds: f64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Target {
    None,
    Init,
    Moof,
    Payload,
    Styp,
    Prefix,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Trex {
    description_index: u32,
    duration: u32,
    size: u32,
    flags: u32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct TrackIds {
    id: u32,
    timescale: u32,
    trex: Trex,
}

#[derive(Debug, Default)]
struct Tfhd {
    track_id: u32,
    sample_description_index: Option<u32>,
    default_duration: u32,
    default_size: u32,
    default_flags: Option<u32>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Sample {
    duration: u32,
    size: u32,
    flags: u32,
    composition_time_offset_raw: u32,
}

#[derive(Debug, Clone, Default, PartialEq)]
struct Run {
    source_data_offset: Option<i32>,
    samples: Vec<Sample>,
    version: u8,
    has_composition_time_offsets: bool,
    duration: u64,
    data_size: u64,
    payload_offset: i64,
    output_offset: i64,
    starts_with_sync: bool,
    has_inferred_duration: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
struct Fragment {
    track_id: u32,
    timescale: u32,
    decode_time: u64,
    duration: u64,
    starts_with_sync: bool,
    sample_description_index: u32,
    tfhd: [u8; CANONICAL_TFHD_SIZE],
    runs: Vec<Run>,
    payload_start: usize,
    payload_len: usize,
    has_inferred_duration: bool,
}

impl Fragment {
    fn end_time(&self) -> u64 {
        self.decode_time + self.duration
    }
}

/// `mp4BoxReader`. The reference reports the init segment and each finished
/// segment through callbacks; here they wait in [`Reader::take_init`] and
/// [`Reader::take_segment`] for the caller.
pub struct Reader {
    segment_seconds: f64,
    segment_diff: i64,
    cue_mode: bool,

    init: Vec<u8>,
    source_moof: Vec<u8>,
    source_styp: Vec<u8>,
    deferred: Vec<u8>,

    video: Vec<Fragment>,
    audio: Vec<Fragment>,

    source_payload: Vec<u8>,
    prefix: Vec<u8>,
    prefix_active: bool,

    pending: Option<Fragment>,
    styp: Vec<u8>,

    box_header: [u8; 16],
    box_header_length: usize,
    box_header_required: usize,

    current_box_type: u32,
    current_box_remaining: u64,
    current_target: Target,

    init_done: bool,
    moov_completed: bool,
    source_mfra_done: bool,
    source_final_moov_done: bool,
    source_payload_from_moof: i64,
    current_payload_start: usize,

    video_track: TrackIds,
    audio_track: TrackIds,

    video_duration_hint: u32,
    video_duration_hint_timescale: u32,
    audio_duration_hint: u32,
    audio_duration_hint_timescale: u32,

    tfdt_offset_seconds: f64,
    sequence: u32,
    last_video_end_time: u64,
    completed_video_segments: u64,

    target_segment: Option<(u64, u64, u64)>,

    new_init: Option<Vec<u8>>,
    new_segment: Option<Segment>,
}

impl Reader {
    pub fn new(segment_seconds: f64, segment_diff: i64, cue_mode: bool) -> Self {
        let segment_seconds = if segment_seconds.is_finite() && segment_seconds > 0.0 {
            segment_seconds
        } else {
            6.0
        };
        Self {
            segment_seconds,
            segment_diff: segment_diff.max(0),
            cue_mode,
            init: Vec::new(),
            source_moof: Vec::new(),
            source_styp: Vec::new(),
            deferred: Vec::new(),
            video: Vec::new(),
            audio: Vec::new(),
            source_payload: Vec::new(),
            prefix: Vec::new(),
            prefix_active: false,
            pending: None,
            styp: Vec::new(),
            box_header: [0; 16],
            box_header_length: 0,
            box_header_required: 8,
            current_box_type: 0,
            current_box_remaining: 0,
            current_target: Target::None,
            init_done: false,
            moov_completed: false,
            source_mfra_done: false,
            source_final_moov_done: false,
            source_payload_from_moof: 0,
            current_payload_start: 0,
            video_track: TrackIds::default(),
            audio_track: TrackIds::default(),
            video_duration_hint: 0,
            video_duration_hint_timescale: 0,
            audio_duration_hint: 0,
            audio_duration_hint_timescale: 0,
            tfdt_offset_seconds: 0.0,
            sequence: 1,
            last_video_end_time: 0,
            completed_video_segments: 0,
            target_segment: None,
            new_init: None,
            new_segment: None,
        }
    }

    /// The init segment, once, when it has been completed.
    pub fn take_init(&mut self) -> Option<Vec<u8>> {
        self.new_init.take()
    }

    /// The segment the last call completed.
    pub fn take_segment(&mut self) -> Option<Segment> {
        self.new_segment.take()
    }

    pub fn has_video(&self) -> bool {
        !self.video.is_empty()
    }

    pub fn video_starts_with_sync(&self) -> bool {
        self.video
            .first()
            .is_some_and(|fragment| fragment.starts_with_sync)
    }

    /// `SeekReset`: a new pipeline run starts a new mp4mux stream.
    pub fn seek_reset(&mut self, seconds: f64) {
        self.init_done = false;
        self.moov_completed = false;
        self.source_mfra_done = false;
        self.source_final_moov_done = false;
        self.video_track = TrackIds::default();
        self.audio_track = TrackIds::default();
        self.sequence = 1;
        self.styp.clear();
        self.video_duration_hint = 0;
        self.video_duration_hint_timescale = 0;
        self.audio_duration_hint = 0;
        self.audio_duration_hint_timescale = 0;
        self.last_video_end_time = 0;
        self.completed_video_segments = 0;
        self.target_segment = None;
        self.tfdt_offset_seconds = if seconds.is_finite() && seconds > 0.0 {
            seconds
        } else {
            0.0
        };
        self.init.clear();
        self.source_moof.clear();
        self.source_styp.clear();
        self.deferred.clear();
        self.clear_source();
        self.video.clear();
        self.audio.clear();
        self.reset_prefix();
        self.reset_box_state();
        self.new_init = None;
        self.new_segment = None;
    }

    /// `SetTimelineOffsetNS`.
    pub fn set_timeline_offset_ns(&mut self, offset_ns: u64) {
        self.tfdt_offset_seconds = if offset_ns == u64::MAX {
            0.0
        } else {
            offset_ns as f64 / 1_000_000_000.0
        };
    }

    /// `SetTargetSegment`: in cue mode the next segment must span this range.
    pub fn set_target_segment(
        &mut self,
        start_ns: u64,
        end_ns: u64,
        tolerance_ns: u64,
    ) -> Result<(), Error> {
        if !self.cue_mode {
            return Ok(());
        }
        if end_ns <= start_ns {
            return Err(other("invalid cue segment range"));
        }
        self.target_segment = Some((start_ns, end_ns, tolerance_ns.max(1)));
        Ok(())
    }

    /// `Push`: at most one segment completes per call; bytes after it wait
    /// until the caller asks again.
    pub fn push(&mut self, data: &[u8]) -> Result<(), Error> {
        if data.is_empty() {
            return Ok(());
        }
        if self.try_process_deferred()? {
            self.deferred.extend_from_slice(data);
            return Ok(());
        }
        let (consumed, completed) = self.process_bytes(data)?;
        if completed && consumed < data.len() {
            self.deferred.extend_from_slice(&data[consumed..]);
        }
        Ok(())
    }

    /// `TryProcessDeferred`.
    pub fn try_process_deferred(&mut self) -> Result<bool, Error> {
        if self.try_build_segment()? {
            return Ok(true);
        }
        if self.deferred.is_empty() {
            return Ok(false);
        }
        let data = std::mem::take(&mut self.deferred);
        let result = self.process_bytes(&data);
        let (consumed, completed) = match result {
            Ok(value) => value,
            Err(error) => {
                self.deferred = data;
                return Err(error);
            }
        };
        if completed {
            self.deferred = data[consumed..].to_vec();
            return Ok(true);
        }
        if consumed != data.len() {
            let length = data.len();
            self.deferred = data;
            return Err(other(format!(
                "mp4 parser consumed {consumed} of {length} deferred bytes"
            )));
        }
        Ok(false)
    }

    /// `TryBuildEndOfStreamRemainder`: what is left after EOS becomes a last
    /// segment, possibly with only one track.
    pub fn try_build_end_of_stream_remainder(&mut self) -> Result<bool, Error> {
        if self.cue_mode && self.target_segment.is_none() {
            return Ok(false);
        }
        let video_count = self.video.len();
        let mut audio_count = self.audio.len();
        if video_count == 0 && audio_count == 0 {
            return Ok(false);
        }
        if video_count > 0 && !self.video[0].starts_with_sync {
            return Ok(false);
        }
        let final_video_segment = video_count > 0;
        if video_count > 0 && audio_count > 0 {
            let video_end = self.video[video_count - 1].end_time();
            let selected = self.select_audio_count(video_end)?;
            if selected > 0 {
                audio_count = selected;
            }
        }
        self.build_segment(video_count, audio_count, true)?;
        if final_video_segment && self.video.is_empty() && !self.audio.is_empty() {
            self.audio.clear();
        }
        Ok(true)
    }

    /// `undecodableEOSRemainderError`.
    pub fn undecodable_remainder_error(&self) -> Error {
        let start = self
            .video
            .first()
            .filter(|fragment| fragment.timescale != 0)
            .map_or(0.0, |fragment| {
                fragment.decode_time as f64 / f64::from(fragment.timescale)
            });
        Error::UndecodableEosRemainder(format!(
            "leftover video starts with a non-sync sample at {start:.6}s"
        ))
    }

    /// `EndOfStreamError`: anything half-read when the stream ended.
    pub fn end_of_stream_error(&self) -> Option<Error> {
        let truncated = |detail: String| Some(Error::TruncatedMp4Fragment(detail));
        if self.box_header_length != 0 {
            return truncated(format!(
                "incomplete top-level box header: have={}, need={}",
                self.box_header_length, self.box_header_required
            ));
        }
        if self.current_box_remaining != 0 {
            return truncated(format!(
                "incomplete {} box: missing={} body bytes",
                four_cc_text(self.current_box_type),
                self.current_box_remaining
            ));
        }
        if let Some(pending) = &self.pending {
            return truncated(format!(
                "moof for track_ID={} has no complete mdat",
                pending.track_id
            ));
        }
        if !self.source_payload.is_empty() {
            return truncated(format!(
                "{} uncommitted mdat payload bytes",
                self.source_payload.len()
            ));
        }
        if !self.source_moof.is_empty() {
            return truncated(format!("{} uncommitted moof bytes", self.source_moof.len()));
        }
        if !self.deferred.is_empty() {
            return truncated(format!(
                "{} unprocessed deferred bytes",
                self.deferred.len()
            ));
        }
        None
    }

    fn process_bytes(&mut self, data: &[u8]) -> Result<(usize, bool), Error> {
        let mut position = 0;
        while position < data.len() {
            if self.box_header_length < self.box_header_required {
                let count =
                    (self.box_header_required - self.box_header_length).min(data.len() - position);
                self.box_header[self.box_header_length..self.box_header_length + count]
                    .copy_from_slice(&data[position..position + count]);
                self.box_header_length += count;
                position += count;
                if self.box_header_length < self.box_header_required {
                    break;
                }
                if self.box_header_required == 8 {
                    let size32 = be32(&self.box_header[0..4]);
                    self.current_box_type = be32(&self.box_header[4..8]);
                    if size32 == 1 {
                        self.box_header_required = 16;
                        continue;
                    }
                    if size32 == 0 {
                        return Err(other("top-level mp4 box size=0 is not supported"));
                    }
                    self.begin_box(u64::from(size32), 8)?;
                } else {
                    let size64 = be64(&self.box_header[8..16]);
                    self.begin_box(size64, 16)?;
                }
                if self.current_box_remaining == 0 {
                    let completed = self.complete_box()?;
                    self.reset_box_state();
                    if completed {
                        return Ok((position, true));
                    }
                }
                continue;
            }
            let body = (data.len() - position)
                .min(usize::try_from(self.current_box_remaining).unwrap_or(usize::MAX));
            if body == 0 {
                break;
            }
            self.write_current_box_data(&data[position..position + body]);
            position += body;
            self.current_box_remaining -= body as u64;
            if self.current_box_remaining == 0 {
                let completed = self.complete_box()?;
                self.reset_box_state();
                if completed {
                    return Ok((position, true));
                }
            }
        }
        Ok((position, false))
    }

    fn begin_box(&mut self, size: u64, header_size: usize) -> Result<(), Error> {
        if size < header_size as u64 {
            return Err(other("invalid mp4 box size"));
        }
        if (self.current_box_type == MOOF || self.current_box_type == MDAT)
            && size > i32::MAX as u64
        {
            return Err(other("moof/mdat is too large"));
        }
        self.current_box_remaining = size - header_size as u64;
        self.current_target = Target::None;

        if !self.init_done && (self.current_box_type == STYP || self.current_box_type == MOOF) {
            self.complete_init()?;
        }
        let header = self.box_header;
        let header = &header[..header_size];
        if !self.init_done {
            if self.current_box_type == MDAT {
                return Err(other("mdat appeared before init was completed"));
            }
            if self.current_box_type == MFRA {
                return Err(other("mfra appeared before init was completed"));
            }
            self.current_target = Target::Init;
            self.write_current_box_data(header);
            return Ok(());
        }
        if self.source_final_moov_done {
            return Err(other(format!(
                "unexpected top-level mp4 box after terminal moov: {}",
                four_cc_text(self.current_box_type)
            )));
        }
        if self.source_mfra_done && self.current_box_type != MOOV {
            return Err(other(format!(
                "only the rewritten terminal moov is allowed after mfra; got {}",
                four_cc_text(self.current_box_type)
            )));
        }
        match self.current_box_type {
            MOOF => {
                if self.pending.is_some() {
                    return Err(other("a new moof appeared before the previous mdat"));
                }
                self.source_moof.clear();
                self.source_payload_from_moof = 0;
                self.current_target = Target::Moof;
                self.write_current_box_data(header);
                Ok(())
            }
            MDAT => {
                if self.pending.is_none() {
                    return Err(other("mdat does not follow a supported moof"));
                }
                self.current_payload_start = self.source_payload.len();
                if self.current_box_remaining <= MAX_SAMPLE_BYTES {
                    self.source_payload
                        .reserve(self.current_box_remaining as usize);
                }
                self.source_payload_from_moof += header_size as i64;
                self.current_target = Target::Payload;
                Ok(())
            }
            SIDX => {
                if self.pending.is_some() {
                    self.source_payload_from_moof += size as i64;
                }
                Ok(())
            }
            STYP => {
                if self.pending.is_some() {
                    return Err(other("styp cannot appear between moof and mdat"));
                }
                self.source_styp.clear();
                self.current_target = Target::Styp;
                self.write_current_box_data(header);
                Ok(())
            }
            EMSG | FREE | PRFT => {
                if self.pending.is_some() {
                    self.source_payload_from_moof += size as i64;
                }
                self.prefix_active = true;
                self.current_target = Target::Prefix;
                self.write_current_box_data(header);
                Ok(())
            }
            MFRA => {
                if self.pending.is_some() {
                    return Err(other("mfra cannot appear between moof and mdat"));
                }
                if self.source_mfra_done {
                    return Err(other("duplicate terminal mfra"));
                }
                Ok(())
            }
            MOOV => {
                if !self.source_mfra_done {
                    return Err(other("unexpected moov after mp4 initialization"));
                }
                if self.source_final_moov_done {
                    return Err(other("duplicate terminal moov"));
                }
                if self.pending.is_some() {
                    return Err(other("final moov cannot appear between moof and mdat"));
                }
                Ok(())
            }
            kind => Err(other(format!(
                "unsupported top-level mp4 box after init: {}",
                four_cc_text(kind)
            ))),
        }
    }

    fn write_current_box_data(&mut self, data: &[u8]) {
        match self.current_target {
            Target::Init => self.init.extend_from_slice(data),
            Target::Moof => self.source_moof.extend_from_slice(data),
            Target::Payload => self.source_payload.extend_from_slice(data),
            Target::Styp => self.source_styp.extend_from_slice(data),
            Target::Prefix => self.prefix.extend_from_slice(data),
            Target::None => {}
        }
    }

    fn complete_box(&mut self) -> Result<bool, Error> {
        match self.current_box_type {
            STYP => {
                if self.styp.is_empty() && !self.source_styp.is_empty() {
                    self.styp = self.source_styp.clone();
                }
                self.source_styp.clear();
                Ok(false)
            }
            MFRA => {
                self.source_mfra_done = true;
                Ok(false)
            }
            MOOV => {
                if self.init_done {
                    if !self.source_mfra_done || self.source_final_moov_done {
                        return Err(other("unexpected moov after mp4 initialization"));
                    }
                    self.source_final_moov_done = true;
                    return Ok(false);
                }
                self.moov_completed = true;
                Ok(false)
            }
            MOOF => {
                self.complete_moof()?;
                Ok(false)
            }
            MDAT => {
                self.complete_mdat()?;
                self.try_build_segment()
            }
            _ => Ok(false),
        }
    }

    fn complete_init(&mut self) -> Result<(), Error> {
        if !self.moov_completed || self.init.is_empty() {
            return Err(other("incomplete mp4 initialization"));
        }
        let init = self.init.clone();
        let (video, audio) = parse_init_tracks(&init)
            .map_err(|error| other(format!("unable to parse mp4 initialization: {error}")))?;
        self.video_track = video;
        self.audio_track = audio;
        self.init_done = true;
        self.new_init = Some(init);
        Ok(())
    }

    fn complete_moof(&mut self) -> Result<(), Error> {
        let video_hint = if self.video_duration_hint_timescale == self.video_track.timescale {
            self.video_duration_hint
        } else {
            0
        };
        let audio_hint = if self.audio_duration_hint_timescale == self.audio_track.timescale {
            self.audio_duration_hint
        } else {
            0
        };
        let mut fragment = parse_source_moof(
            &self.source_moof,
            self.video_track,
            self.audio_track,
            video_hint,
            audio_hint,
        )
        .map_err(|error| other(format!("unable to parse source moof: {error}")))?;
        self.resolve_previous_inferred_duration(&fragment)?;
        if !fragment.has_inferred_duration {
            let duration = last_sample_duration(&fragment);
            if duration != 0 {
                if fragment.track_id == self.video_track.id {
                    self.video_duration_hint = duration;
                    self.video_duration_hint_timescale = fragment.timescale;
                } else if fragment.track_id == self.audio_track.id {
                    self.audio_duration_hint = duration;
                    self.audio_duration_hint_timescale = fragment.timescale;
                }
            }
        }
        fragment.payload_start = 0;
        self.pending = Some(fragment);
        self.source_payload_from_moof = self.source_moof.len() as i64;
        Ok(())
    }

    fn resolve_previous_inferred_duration(&mut self, current: &Fragment) -> Result<(), Error> {
        let fragments = if current.track_id == self.video_track.id {
            &mut self.video
        } else if current.track_id == self.audio_track.id {
            &mut self.audio
        } else {
            return Ok(());
        };
        let Some(previous) = fragments.last_mut() else {
            return Ok(());
        };
        if !previous.has_inferred_duration {
            return Ok(());
        }
        let decode_time = previous.decode_time;
        let previous_duration = previous.duration;
        let Some(run) = previous
            .runs
            .iter_mut()
            .rev()
            .find(|run| run.has_inferred_duration)
            .filter(|run| !run.samples.is_empty())
        else {
            return Err(other("inferred sample duration marker is missing"));
        };
        let index = run.samples.len() - 1;
        let old = u64::from(run.samples[index].duration);
        if previous_duration < old {
            return Err(other("invalid inferred sample duration"));
        }
        let sample_start = decode_time + previous_duration - old;
        if current.decode_time < sample_start {
            return Err(other(format!(
                "inferred sample moves decode time backwards: track={} previous_tfdt={} next_tfdt={}",
                current.track_id, decode_time, current.decode_time
            )));
        }
        let exact = current.decode_time - sample_start;
        let Ok(exact32) = u32::try_from(exact) else {
            return Err(other("inferred sample duration exceeds uint32"));
        };
        run.samples[index].duration = exact32;
        run.duration = run.duration - old + exact;
        previous.duration = previous_duration - old + exact;
        for run in &mut previous.runs {
            run.has_inferred_duration = false;
        }
        previous.has_inferred_duration = false;
        if exact != 0 {
            if current.track_id == self.video_track.id {
                self.video_duration_hint = exact32;
                self.video_duration_hint_timescale = current.timescale;
            } else {
                self.audio_duration_hint = exact32;
                self.audio_duration_hint_timescale = current.timescale;
            }
        }
        Ok(())
    }

    fn complete_mdat(&mut self) -> Result<(), Error> {
        let Some(mut pending) = self.pending.take() else {
            return Err(other("completed mdat has no source moof"));
        };
        let payload_len = self.source_payload.len() - self.current_payload_start;
        attach_payload(
            &mut pending,
            self.current_payload_start,
            payload_len,
            self.source_payload_from_moof,
        )?;
        if pending.track_id == self.video_track.id {
            self.video.push(pending);
        } else if self.audio_track.id != 0 && pending.track_id == self.audio_track.id {
            self.audio.push(pending);
        } else {
            return Err(other(format!("unsupported track_ID={}", pending.track_id)));
        }
        self.source_payload_from_moof = 0;
        self.current_payload_start = 0;
        self.source_moof.clear();
        Ok(())
    }

    fn try_build_segment(&mut self) -> Result<bool, Error> {
        let video_count = self.select_video_count()?;
        if video_count == 0 {
            return Ok(false);
        }
        let mut audio_count = 0;
        if self.audio_track.id != 0 {
            let video_end = self.video[video_count - 1].end_time();
            audio_count = self.select_audio_count(video_end)?;
            if audio_count == 0 {
                return Ok(false);
            }
        }
        self.build_segment(video_count, audio_count, false)?;
        Ok(true)
    }

    fn select_video_count(&self) -> Result<usize, Error> {
        let Some(first) = self.video.first() else {
            return Ok(0);
        };
        if !first.starts_with_sync {
            return Err(other(format!(
                "video segment starts with a non-sync sample at {:.6}s",
                first.decode_time as f64 / f64::from(self.video_track.timescale)
            )));
        }
        if self.cue_mode {
            return self.select_cue_video_count();
        }
        let target = to_units(self.segment_seconds, self.video_track.timescale)?;
        let mut take_first_sync_boundary = false;
        if self.completed_video_segments > 0 && self.segment_diff > 0 {
            let timescale = u64::from(self.video_track.timescale);
            let expected_end = self
                .completed_video_segments
                .wrapping_mul(self.segment_seconds as u64)
                .wrapping_mul(timescale);
            let diff =
                ((self.segment_seconds + self.segment_diff as f64) as u64).wrapping_mul(timescale);
            if self.last_video_end_time > expected_end
                && self.last_video_end_time - expected_end >= diff
            {
                take_first_sync_boundary = true;
            }
        }
        let mut duration = 0u64;
        let mut selected = 0;
        for index in 0..self.video.len().saturating_sub(1) {
            duration += self.video[index].duration;
            let next_sync = self.video[index + 1].starts_with_sync;
            if !take_first_sync_boundary {
                if duration >= target && next_sync {
                    return Ok(index + 1);
                }
                continue;
            }
            if duration <= target {
                if next_sync {
                    selected = index + 1;
                    if duration == target {
                        return Ok(selected);
                    }
                }
                continue;
            }
            if selected > 0 {
                return Ok(selected);
            }
            if next_sync {
                return Ok(index + 1);
            }
        }
        Ok(0)
    }

    fn select_cue_video_count(&self) -> Result<usize, Error> {
        let Some((start_ns, end_ns, tolerance_ns)) = self.target_segment else {
            return Ok(0);
        };
        if self.video.len() < 2 {
            return Ok(0);
        }
        let first = first_presentation_time(&self.video[0])?;
        let target = end_ns - start_ns;
        for (index, boundary) in self.video.iter().enumerate().skip(1) {
            if !boundary.starts_with_sync {
                continue;
            }
            let presentation = first_presentation_time(boundary)?;
            if presentation <= first {
                return Err(other("cue presentation timeline is not increasing"));
            }
            let duration =
                timeline_nanoseconds((presentation - first) as u64, self.video_track.timescale)?;
            if duration < target && target - duration > tolerance_ns {
                continue;
            }
            if duration > target && duration - target > tolerance_ns {
                return Err(other(format!(
                    "cue sync boundary duration is {duration}, expected {target}"
                )));
            }
            return Ok(index);
        }
        Ok(0)
    }

    fn select_audio_count(&mut self, video_end: u64) -> Result<usize, Error> {
        for index in 0..self.audio.len() {
            let (split, reached) =
                self.find_audio_split_sample_count(&self.audio[index], video_end)?;
            if !reached {
                continue;
            }
            let total = fragment_sample_count(&self.audio[index])?;
            if split == 0 || split > total {
                return Err(other("invalid audio split sample count"));
            }
            if split < total {
                let (left, right) = split_fragment_at_sample(&self.audio[index], split)?;
                self.audio[index] = left;
                self.audio.insert(index + 1, right);
            }
            return Ok(index + 1);
        }
        Ok(0)
    }

    fn find_audio_split_sample_count(
        &self,
        fragment: &Fragment,
        video_end: u64,
    ) -> Result<(usize, bool), Error> {
        if fragment.timescale == 0 || self.video_track.timescale == 0 {
            return Err(other("audio/video timescale is zero"));
        }
        let mut audio_end = fragment.decode_time;
        let mut count = 0;
        for run in &fragment.runs {
            if run.samples.is_empty() {
                return Err(other("audio trun has no parsed samples"));
            }
            for sample in &run.samples {
                audio_end = audio_end
                    .checked_add(u64::from(sample.duration))
                    .ok_or_else(|| other("audio sample timeline overflow"))?;
                count += 1;
                if scaled_greater_or_equal(
                    audio_end,
                    self.video_track.timescale,
                    video_end,
                    fragment.timescale,
                ) {
                    return Ok((count, true));
                }
            }
        }
        if audio_end != fragment.end_time() {
            return Err(other(
                "audio sample durations do not match fragment duration",
            ));
        }
        Ok((count, false))
    }

    fn build_segment(
        &mut self,
        video_count: usize,
        audio_count: usize,
        allow_single_track: bool,
    ) -> Result<(), Error> {
        if video_count > self.video.len() {
            return Err(other("invalid video fragment count"));
        }
        if audio_count > self.audio.len() {
            return Err(other("invalid audio fragment count"));
        }
        let has_video = video_count > 0;
        let has_audio = audio_count > 0;
        let source_has_audio = self.audio_track.id != 0;
        if !has_video && !has_audio {
            return Err(other("segment contains no fragments"));
        }
        if has_audio && !source_has_audio {
            return Err(other(
                "video-only source unexpectedly contains audio fragments",
            ));
        }
        if !allow_single_track {
            if !has_video {
                return Err(other("regular segment must contain video"));
            }
            if source_has_audio && !has_audio {
                return Err(other(
                    "regular segment must contain audio for an audio/video source",
                ));
            }
        }
        if has_video {
            validate_track(&self.video, video_count)?;
        }
        if has_audio {
            validate_track(&self.audio, audio_count)?;
        }

        let mut payload_length = 0i64;
        if has_video {
            assign_offsets(&mut self.video, video_count, &mut payload_length);
        }
        if has_audio {
            assign_offsets(&mut self.audio, audio_count, &mut payload_length);
        }
        let video_traf = if has_video {
            traf_size(&self.video, video_count)
        } else {
            0
        };
        let audio_traf = if has_audio {
            traf_size(&self.audio, audio_count)
        } else {
            0
        };
        let moof_size = 8 + 16 + video_traf + audio_traf;
        let Ok(moof_size) = u32::try_from(moof_size) else {
            return Err(other("combined moof is too large"));
        };
        let mdat_header_size = if payload_length as u64 + 8 > u64::from(u32::MAX) {
            16
        } else {
            8
        };

        let mut data = Vec::with_capacity(
            moof_size as usize
                + mdat_header_size
                + self.styp.len()
                + self.prefix.len()
                + payload_length as usize,
        );
        data.extend_from_slice(&self.styp);
        if self.prefix_active {
            data.extend_from_slice(&self.prefix);
        }
        write_header(&mut data, moof_size, MOOF);
        write_mfhd(&mut data, self.sequence);
        self.sequence = self.sequence.wrapping_add(1);
        if has_video {
            self.write_traf(
                &mut data,
                &self.video,
                video_count,
                moof_size,
                mdat_header_size,
            )?;
        }
        if has_audio {
            self.write_traf(
                &mut data,
                &self.audio,
                audio_count,
                moof_size,
                mdat_header_size,
            )?;
        }
        write_mdat_header(&mut data, payload_length as u64, mdat_header_size);

        let (first, last) = if has_video {
            (&self.video[0], &self.video[video_count - 1])
        } else {
            (&self.audio[0], &self.audio[audio_count - 1])
        };
        if first.timescale == 0 {
            return Err(other("segment first track has zero timescale"));
        }
        if last.timescale == 0 {
            return Err(other("segment last track has zero timescale"));
        }
        let mut start_ns = timeline_nanoseconds(first.decode_time, first.timescale)?;
        let mut end_ns = timeline_nanoseconds(last.end_time(), last.timescale)?;
        let offset_ns = if self.tfdt_offset_seconds > 0.0 {
            (self.tfdt_offset_seconds * 1_000_000_000.0).round() as u64
        } else {
            0
        };
        if u64::MAX - start_ns < offset_ns || u64::MAX - end_ns < offset_ns {
            return Err(other("segment timeline offset overflow"));
        }
        start_ns += offset_ns;
        end_ns += offset_ns;
        for fragment in self.video[..video_count]
            .iter()
            .chain(&self.audio[..audio_count])
        {
            if fragment.payload_len == 0 {
                continue;
            }
            let end = fragment.payload_start + fragment.payload_len;
            let Some(payload) = self.source_payload.get(fragment.payload_start..end) else {
                return Err(other("fragment payload range exceeds storage"));
            };
            data.extend_from_slice(payload);
        }
        let last_video_end = has_video.then(|| self.video[video_count - 1].end_time());

        self.new_segment = Some(Segment {
            data,
            start_ns,
            end_ns,
            start_seconds: start_ns as f64 / 1_000_000_000.0,
            end_seconds: end_ns as f64 / 1_000_000_000.0,
        });
        if self.cue_mode {
            self.target_segment = None;
        }
        if let Some(end) = last_video_end {
            self.last_video_end_time = end;
            self.completed_video_segments += 1;
            self.video.drain(..video_count);
        }
        if has_audio {
            self.audio.drain(..audio_count);
        }
        self.reset_prefix();
        self.reclaim_payloads();
        Ok(())
    }

    fn write_traf(
        &self,
        output: &mut Vec<u8>,
        fragments: &[Fragment],
        count: usize,
        moof_size: u32,
        mdat_header_size: usize,
    ) -> Result<(), Error> {
        let size = traf_size(fragments, count);
        let Ok(size) = u32::try_from(size) else {
            return Err(other("combined traf is too large"));
        };
        let first = &fragments[0];
        write_header(output, size, TRAF);
        output.extend_from_slice(&first.tfhd);
        let decode_time =
            add_tfdt_offset(first.decode_time, first.timescale, self.tfdt_offset_seconds)?;
        write_tfdt(output, decode_time);
        for fragment in &fragments[..count] {
            for run in &fragment.runs {
                let offset = i64::from(moof_size) + mdat_header_size as i64 + run.output_offset;
                let Ok(offset) = i32::try_from(offset) else {
                    return Err(other("trun.data_offset exceeds int32"));
                };
                write_canonical_trun(output, run, offset)?;
            }
        }
        Ok(())
    }

    /// `ReclaimPayloads`: drops payload bytes no queued fragment uses.
    fn reclaim_payloads(&mut self) {
        if self.source_payload.is_empty() {
            return;
        }
        let starts = self
            .video
            .iter()
            .chain(&self.audio)
            .filter(|fragment| fragment.payload_len > 0)
            .map(|fragment| fragment.payload_start);
        let Some(min_start) = starts.min() else {
            self.source_payload.clear();
            self.current_payload_start = 0;
            return;
        };
        if min_start == 0 {
            return;
        }
        self.source_payload.drain(..min_start);
        for fragment in self.video.iter_mut().chain(&mut self.audio) {
            if fragment.payload_len > 0 {
                fragment.payload_start -= min_start;
            }
        }
        self.current_payload_start = self.current_payload_start.saturating_sub(min_start);
    }

    fn reset_prefix(&mut self) {
        self.prefix.clear();
        self.prefix_active = false;
    }

    fn clear_source(&mut self) {
        self.pending = None;
        self.source_payload.clear();
        self.source_payload_from_moof = 0;
        self.current_payload_start = 0;
        self.source_moof.clear();
    }

    fn reset_box_state(&mut self) {
        self.box_header_length = 0;
        self.box_header_required = 8;
        self.current_box_type = 0;
        self.current_box_remaining = 0;
        self.current_target = Target::None;
    }
}

fn last_sample_duration(fragment: &Fragment) -> u32 {
    fragment
        .runs
        .iter()
        .rev()
        .flat_map(|run| run.samples.iter().rev())
        .map(|sample| sample.duration)
        .find(|duration| *duration != 0)
        .unwrap_or(0)
}

fn attach_payload(
    fragment: &mut Fragment,
    payload_start: usize,
    payload_len: usize,
    payload_from_moof: i64,
) -> Result<(), Error> {
    let mut expected = 0i64;
    for run in &mut fragment.runs {
        let offset = run
            .source_data_offset
            .map_or(expected, |offset| i64::from(offset) - payload_from_moof);
        if offset != expected {
            return Err(other(format!(
                "non-contiguous source mdat: expected={expected}, actual={offset}"
            )));
        }
        if run.data_size > i64::MAX as u64 {
            return Err(other("trun payload is too large"));
        }
        run.payload_offset = offset;
        expected = offset + run.data_size as i64;
    }
    if expected != payload_len as i64 {
        return Err(other(format!(
            "source mdat size mismatch: trun={expected}, mdat={payload_len}"
        )));
    }
    fragment.payload_start = payload_start;
    fragment.payload_len = payload_len;
    Ok(())
}

fn first_presentation_time(fragment: &Fragment) -> Result<i64, Error> {
    let Some(run) = fragment.runs.first() else {
        return Err(other("video fragment has no first sample"));
    };
    let Some(sample) = run.samples.first() else {
        return Err(other("video fragment has no first sample"));
    };
    let offset = if run.version == 1 {
        i64::from(sample.composition_time_offset_raw as i32)
    } else {
        i64::from(sample.composition_time_offset_raw)
    };
    let Ok(decode) = i64::try_from(fragment.decode_time) else {
        return Err(other("video decode time exceeds int64"));
    };
    decode.checked_add(offset).ok_or_else(|| {
        other(if offset > 0 {
            "presentation time overflow"
        } else {
            "presentation time underflow"
        })
    })
}

fn fragment_sample_count(fragment: &Fragment) -> Result<usize, Error> {
    let mut total = 0;
    for run in &fragment.runs {
        if run.samples.is_empty() {
            return Err(other("fragment trun has no parsed samples"));
        }
        total += run.samples.len();
    }
    if total == 0 {
        return Err(other("fragment contains no samples"));
    }
    Ok(total)
}

/// `splitFragmentAtSample`: audio is cut at the first sample reaching the
/// video's end; both halves keep explicit runs.
fn split_fragment_at_sample(
    fragment: &Fragment,
    split: usize,
) -> Result<(Fragment, Fragment), Error> {
    let total = fragment_sample_count(fragment)?;
    if split == 0 || split >= total {
        return Err(other(format!(
            "split sample must be inside fragment: split={split} total={total}"
        )));
    }
    let template = Fragment {
        track_id: fragment.track_id,
        timescale: fragment.timescale,
        sample_description_index: fragment.sample_description_index,
        tfhd: fragment.tfhd,
        ..Fragment::default()
    };
    let mut left = Fragment {
        decode_time: fragment.decode_time,
        ..template.clone()
    };
    let mut right = template;
    let mut remaining = split;
    let (mut left_bytes, mut right_bytes, mut left_duration, mut right_duration) =
        (0u64, 0u64, 0u64, 0u64);
    for source in &fragment.runs {
        if source.samples.is_empty() {
            return Err(other("cannot split a run without parsed samples"));
        }
        let take = remaining.min(source.samples.len());
        if take > 0 {
            let mut run = build_explicit_run(source, &source.samples[..take])?;
            run.payload_offset = left_bytes as i64;
            left_bytes += run.data_size;
            left_duration += run.duration;
            left.runs.push(run);
            remaining -= take;
        }
        if take < source.samples.len() {
            let mut run = build_explicit_run(source, &source.samples[take..])?;
            run.payload_offset = right_bytes as i64;
            right_bytes += run.data_size;
            right_duration += run.duration;
            right.runs.push(run);
        }
    }
    if remaining != 0 || left.runs.is_empty() || right.runs.is_empty() {
        return Err(other("failed to partition fragment runs"));
    }
    if left_duration + right_duration != fragment.duration {
        return Err(other("split fragment duration mismatch"));
    }
    if left_bytes + right_bytes != fragment.payload_len as u64 {
        return Err(other(format!(
            "split payload mismatch: left={left_bytes} right={right_bytes} source={}",
            fragment.payload_len
        )));
    }
    let split_byte = left_bytes as usize;
    left.duration = left_duration;
    left.starts_with_sync = left.runs[0].starts_with_sync;
    left.payload_start = fragment.payload_start;
    left.payload_len = split_byte;
    right.decode_time = fragment
        .decode_time
        .checked_add(left_duration)
        .ok_or_else(|| other("right fragment decode time overflow"))?;
    right.duration = right_duration;
    right.starts_with_sync = right.runs[0].starts_with_sync;
    right.payload_start = fragment.payload_start + split_byte;
    right.payload_len = fragment.payload_len - split_byte;
    Ok((left, right))
}

fn validate_track(fragments: &[Fragment], count: usize) -> Result<(), Error> {
    if count == 0 || count > fragments.len() {
        return Err(other("invalid fragment count"));
    }
    let first = &fragments[0];
    let mut expected = first.end_time();
    for current in &fragments[1..count] {
        if current.track_id != first.track_id
            || current.timescale != first.timescale
            || current.decode_time != expected
            || current.sample_description_index != first.sample_description_index
            || current.tfhd != first.tfhd
        {
            return Err(other(format!(
                "track {} fragments cannot be merged into one traf",
                first.track_id
            )));
        }
        expected = current.end_time();
    }
    Ok(())
}

fn assign_offsets(fragments: &mut [Fragment], count: usize, output_offset: &mut i64) {
    for fragment in &mut fragments[..count] {
        let base = *output_offset;
        for run in &mut fragment.runs {
            run.output_offset = base + run.payload_offset;
        }
        *output_offset += fragment.payload_len as i64;
    }
}

fn traf_size(fragments: &[Fragment], count: usize) -> i64 {
    let runs: i64 = fragments[..count]
        .iter()
        .flat_map(|fragment| &fragment.runs)
        .map(canonical_trun_size)
        .sum();
    (8 + CANONICAL_TFHD_SIZE + 20) as i64 + runs
}

fn canonical_trun_size(run: &Run) -> i64 {
    let entry = if run.has_composition_time_offsets {
        16
    } else {
        12
    };
    20 + run.samples.len() as i64 * entry
}

fn write_canonical_trun(output: &mut Vec<u8>, run: &Run, data_offset: i32) -> Result<(), Error> {
    if run.samples.is_empty() {
        return Err(other("cannot write trun without samples"));
    }
    if run.version > 1 {
        return Err(other(format!("unsupported trun version={}", run.version)));
    }
    let Ok(size) = u32::try_from(canonical_trun_size(run)) else {
        return Err(other("canonical trun is too large"));
    };
    let mut flags = TRUN_DATA_OFFSET | TRUN_DURATION | TRUN_SIZE | TRUN_FLAGS;
    if run.has_composition_time_offsets {
        flags |= TRUN_COMPOSITION_OFFSET;
    }
    output.extend_from_slice(&size.to_be_bytes());
    output.extend_from_slice(&TRUN.to_be_bytes());
    output.extend_from_slice(&(u32::from(run.version) << 24 | flags).to_be_bytes());
    output.extend_from_slice(&(run.samples.len() as u32).to_be_bytes());
    output.extend_from_slice(&data_offset.to_be_bytes());
    for sample in &run.samples {
        output.extend_from_slice(&sample.duration.to_be_bytes());
        output.extend_from_slice(&sample.size.to_be_bytes());
        output.extend_from_slice(&sample.flags.to_be_bytes());
        if run.has_composition_time_offsets {
            output.extend_from_slice(&sample.composition_time_offset_raw.to_be_bytes());
        }
    }
    Ok(())
}

fn parse_source_moof(
    moof: &[u8],
    video: TrackIds,
    audio: TrackIds,
    video_hint: u32,
    audio_hint: u32,
) -> Result<Fragment, String> {
    let mut root = 0;
    let Some((kind, header, moof_box)) = try_read_box(moof, &mut root) else {
        return Err("buffer does not contain exactly one moof".into());
    };
    if kind != MOOF || root != moof.len() {
        return Err("buffer does not contain exactly one moof".into());
    }
    let mut position = header;
    let mut traf_count = 0;
    let mut result = Fragment::default();
    while let Some((kind, header, child)) = try_read_box(moof_box, &mut position) {
        if kind != TRAF {
            continue;
        }
        traf_count += 1;
        if traf_count > 1 {
            return Err("source moof must contain one traf".into());
        }
        result = parse_traf(child, header, video, audio, video_hint, audio_hint)?;
    }
    if traf_count == 0 {
        return Err("traf was not found".into());
    }
    Ok(result)
}

fn parse_traf(
    traf: &[u8],
    traf_header: usize,
    video: TrackIds,
    audio: TrackIds,
    video_hint: u32,
    audio_hint: u32,
) -> Result<Fragment, String> {
    let mut tfhd = None;
    let mut decode_time = None;
    let mut position = traf_header;
    while let Some((kind, header, child)) = try_read_box(traf, &mut position) {
        match kind {
            TFHD => {
                if tfhd.is_some() {
                    return Err("duplicate tfhd".into());
                }
                tfhd = Some(parse_tfhd(child, header)?);
            }
            TFDT => {
                if decode_time.is_some() {
                    return Err("duplicate tfdt".into());
                }
                decode_time = Some(read_tfdt(child, header).ok_or("invalid tfdt")?);
            }
            TRUN => {}
            other => {
                return Err(format!(
                    "unsupported box {} inside traf",
                    four_cc_text(other)
                ));
            }
        }
    }
    let (Some(tfhd), Some(decode_time)) = (tfhd, decode_time) else {
        return Err("tfhd/tfdt was not found".into());
    };
    let (timescale, trex, hint) = if tfhd.track_id == video.id {
        (video.timescale, video.trex, video_hint)
    } else if audio.id != 0 && tfhd.track_id == audio.id {
        (audio.timescale, audio.trex, audio_hint)
    } else {
        return Err(format!("unsupported track_ID={}", tfhd.track_id));
    };
    if timescale == 0 {
        return Err(format!("timescale is zero for track_ID={}", tfhd.track_id));
    }
    let description_index = tfhd
        .sample_description_index
        .unwrap_or(trex.description_index);
    if description_index == 0 {
        return Err(format!(
            "sample description index is absent for track_ID={}",
            tfhd.track_id
        ));
    }
    let mut default_duration = tfhd.default_duration;
    let mut duration_is_hint = false;
    if default_duration == 0 {
        default_duration = trex.duration;
    }
    if default_duration == 0 {
        default_duration = hint;
        duration_is_hint = default_duration != 0;
    }
    let default_size = if tfhd.default_size == 0 {
        trex.size
    } else {
        tfhd.default_size
    };
    let default_flags = tfhd.default_flags.unwrap_or(trex.flags);

    let mut fragment = Fragment {
        track_id: tfhd.track_id,
        timescale,
        decode_time,
        sample_description_index: description_index,
        tfhd: canonical_tfhd(tfhd.track_id, description_index),
        ..Fragment::default()
    };
    let mut duration = 0u64;
    let mut position = traf_header;
    while let Some((kind, header, child)) = try_read_box(traf, &mut position) {
        if kind != TRUN {
            continue;
        }
        let run = normalize_trun(
            child,
            header,
            default_duration,
            default_size,
            default_flags,
            duration_is_hint,
        )?;
        duration = duration
            .checked_add(run.duration)
            .ok_or("fragment duration overflow")?;
        fragment.has_inferred_duration |= run.has_inferred_duration;
        fragment.runs.push(run);
    }
    if fragment.runs.is_empty() || duration == 0 {
        return Err("trun/duration was not found".into());
    }
    fragment.duration = duration;
    fragment.starts_with_sync = fragment.runs[0].starts_with_sync;
    Ok(fragment)
}

fn parse_tfhd(data: &[u8], header: usize) -> Result<Tfhd, String> {
    if data.len() < header + 8 {
        return Err("tfhd is too small".into());
    }
    let version_flags = be32(&data[header..]);
    let version = version_flags >> 24;
    let flags = version_flags & 0x00ff_ffff;
    if version != 0 {
        return Err(format!("unsupported tfhd version={version}"));
    }
    let known = TFHD_BASE_DATA_OFFSET
        | TFHD_SAMPLE_DESCRIPTION_INDEX
        | TFHD_DEFAULT_DURATION
        | TFHD_DEFAULT_SIZE
        | TFHD_DEFAULT_FLAGS
        | TFHD_DURATION_IS_EMPTY
        | TFHD_DEFAULT_BASE_IS_MOOF;
    let unknown = flags & !known;
    if unknown != 0 {
        return Err(format!("unsupported tfhd flags=0x{unknown:06x}"));
    }
    if flags & TFHD_BASE_DATA_OFFSET != 0 {
        return Err("tfhd.base-data-offset-present is not supported".into());
    }
    if flags & TFHD_DURATION_IS_EMPTY != 0 {
        return Err("tfhd.duration-is-empty is not supported".into());
    }
    let mut info = Tfhd {
        track_id: be32(&data[header + 4..]),
        ..Tfhd::default()
    };
    if info.track_id == 0 {
        return Err("tfhd track_ID is zero".into());
    }
    let mut cursor = header + 8;
    if flags & TFHD_SAMPLE_DESCRIPTION_INDEX != 0 {
        match read_u32(data, &mut cursor) {
            Some(value) if value != 0 => info.sample_description_index = Some(value),
            _ => return Err("invalid tfhd sample_description_index".into()),
        }
    }
    if flags & TFHD_DEFAULT_DURATION != 0 {
        info.default_duration =
            read_u32(data, &mut cursor).ok_or("invalid tfhd default_sample_duration")?;
    }
    if flags & TFHD_DEFAULT_SIZE != 0 {
        info.default_size =
            read_u32(data, &mut cursor).ok_or("invalid tfhd default_sample_size")?;
    }
    if flags & TFHD_DEFAULT_FLAGS != 0 {
        info.default_flags =
            Some(read_u32(data, &mut cursor).ok_or("invalid tfhd default_sample_flags")?);
    }
    if cursor != data.len() {
        return Err("invalid tfhd body".into());
    }
    Ok(info)
}

fn canonical_tfhd(track_id: u32, description_index: u32) -> [u8; CANONICAL_TFHD_SIZE] {
    let mut tfhd = [0; CANONICAL_TFHD_SIZE];
    tfhd[0..4].copy_from_slice(&(CANONICAL_TFHD_SIZE as u32).to_be_bytes());
    tfhd[4..8].copy_from_slice(&TFHD.to_be_bytes());
    tfhd[8..12].copy_from_slice(
        &(TFHD_SAMPLE_DESCRIPTION_INDEX | TFHD_DEFAULT_BASE_IS_MOOF).to_be_bytes(),
    );
    tfhd[12..16].copy_from_slice(&track_id.to_be_bytes());
    tfhd[16..20].copy_from_slice(&description_index.to_be_bytes());
    tfhd
}

fn normalize_trun(
    data: &[u8],
    header: usize,
    default_duration: u32,
    default_size: u32,
    default_flags: u32,
    duration_is_hint: bool,
) -> Result<Run, String> {
    if data.len() < header + 8 {
        return Err("trun is too small".into());
    }
    let version_flags = be32(&data[header..]);
    let version = (version_flags >> 24) as u8;
    let flags = version_flags & 0x00ff_ffff;
    if version > 1 {
        return Err(format!("unsupported trun version={version}"));
    }
    let known = TRUN_DATA_OFFSET
        | TRUN_FIRST_SAMPLE_FLAGS
        | TRUN_DURATION
        | TRUN_SIZE
        | TRUN_FLAGS
        | TRUN_COMPOSITION_OFFSET;
    let unknown = flags & !known;
    if unknown != 0 {
        return Err(format!("unsupported trun flags=0x{unknown:06x}"));
    }
    let sample_count = be32(&data[header + 4..]);
    if sample_count == 0 {
        return Err("trun sample_count is zero".into());
    }
    let mut run = Run {
        version,
        ..Run::default()
    };
    let mut cursor = header + 8;
    if flags & TRUN_DATA_OFFSET != 0 {
        if data.len() - cursor < 4 {
            return Err("invalid trun data_offset".into());
        }
        run.source_data_offset = Some(be32(&data[cursor..]) as i32);
        cursor += 4;
    }
    let has_first_flags = flags & TRUN_FIRST_SAMPLE_FLAGS != 0;
    let has_flags = flags & TRUN_FLAGS != 0;
    if has_first_flags && has_flags {
        return Err("trun first_sample_flags and sample_flags are both present".into());
    }
    let mut first_flags = default_flags;
    if has_first_flags {
        first_flags = read_u32(data, &mut cursor).ok_or("invalid trun first_sample_flags")?;
    }
    let has_duration = flags & TRUN_DURATION != 0;
    let has_size = flags & TRUN_SIZE != 0;
    let has_offsets = flags & TRUN_COMPOSITION_OFFSET != 0;
    if !has_duration && default_duration == 0 {
        return Err("sample duration is absent".into());
    }
    if !has_size && default_size == 0 {
        return Err("sample size is absent".into());
    }
    if sample_count > MAX_TRUN_SAMPLES {
        return Err(format!(
            "trun sample_count exceeds safety limit: {sample_count}"
        ));
    }
    let fields = [has_duration, has_size, has_flags, has_offsets]
        .iter()
        .filter(|present| **present)
        .count() as u64;
    if cursor as u64 + u64::from(sample_count) * fields * 4 != data.len() as u64 {
        return Err("invalid trun body".into());
    }
    run.samples.reserve(sample_count as usize);
    let mut first_effective_flags = 0;
    for index in 0..sample_count {
        let mut sample = Sample {
            duration: default_duration,
            size: default_size,
            flags: default_flags,
            composition_time_offset_raw: 0,
        };
        if has_duration {
            sample.duration = read_u32(data, &mut cursor).ok_or("invalid trun sample_duration")?;
        }
        if has_size {
            sample.size = read_u32(data, &mut cursor).ok_or("invalid trun sample_size")?;
        }
        if has_flags {
            sample.flags = read_u32(data, &mut cursor).ok_or("invalid trun sample_flags")?;
        } else if index == 0 && has_first_flags {
            sample.flags = first_flags;
        }
        if has_offsets {
            sample.composition_time_offset_raw =
                read_u32(data, &mut cursor).ok_or("invalid trun composition_time_offset")?;
        }
        run.duration = run
            .duration
            .checked_add(u64::from(sample.duration))
            .ok_or("trun duration overflow")?;
        run.data_size = run
            .data_size
            .checked_add(u64::from(sample.size))
            .ok_or("trun payload size overflow")?;
        if index == 0 {
            first_effective_flags = sample.flags;
        }
        run.samples.push(sample);
    }
    if cursor != data.len() || run.duration == 0 || run.data_size == 0 {
        return Err("invalid trun body".into());
    }
    run.has_composition_time_offsets = has_offsets;
    run.starts_with_sync = is_sync_sample(first_effective_flags);
    run.has_inferred_duration = !has_duration && duration_is_hint;
    Ok(run)
}

fn build_explicit_run(template: &Run, samples: &[Sample]) -> Result<Run, Error> {
    if samples.is_empty() {
        return Err(other("cannot build trun without samples"));
    }
    let mut run = Run {
        samples: samples.to_vec(),
        version: template.version,
        has_composition_time_offsets: template.has_composition_time_offsets,
        starts_with_sync: is_sync_sample(samples[0].flags),
        ..Run::default()
    };
    for sample in samples {
        run.duration = run
            .duration
            .checked_add(u64::from(sample.duration))
            .ok_or_else(|| other("derived trun duration overflow"))?;
        run.data_size = run
            .data_size
            .checked_add(u64::from(sample.size))
            .ok_or_else(|| other("derived trun payload size overflow"))?;
    }
    if run.duration == 0 || run.data_size == 0 {
        return Err(other("invalid derived trun"));
    }
    Ok(run)
}

fn is_sync_sample(flags: u32) -> bool {
    const NON_SYNC: u32 = 0x0001_0000;
    let depends_on = (flags >> 24) & 0x03;
    flags & NON_SYNC == 0 && depends_on != 1
}

fn read_tfdt(data: &[u8], header: usize) -> Option<u64> {
    if data.len() < header + 8 {
        return None;
    }
    let offset = header + 4;
    match data[header] {
        1 => (data.len() >= offset + 8).then(|| be64(&data[offset..])),
        0 => Some(u64::from(be32(&data[offset..]))),
        _ => None,
    }
}

fn parse_init_tracks(init: &[u8]) -> Result<(TrackIds, TrackIds), String> {
    let (moov, moov_header) = find_box(init, MOOV).ok_or("moov was not found")?;
    let (mut video, mut audio) = ((0u32, 0u32), (0u32, 0u32));
    let mut trex_entries = Vec::new();
    let mut position = moov_header;
    while let Some((kind, header, child)) = try_read_box(moov, &mut position) {
        if kind == TRAK {
            let (track_id, timescale, handler) = read_track(child, header);
            match handler {
                HANDLER_VIDEO => {
                    if video.0 != 0 {
                        return Err("multiple video tracks in mp4mux output".into());
                    }
                    video = (track_id, timescale);
                }
                HANDLER_AUDIO => {
                    if audio.0 != 0 {
                        return Err("multiple audio tracks in mp4mux output".into());
                    }
                    audio = (track_id, timescale);
                }
                _ => {}
            }
            continue;
        }
        if kind != MVEX {
            continue;
        }
        let mut mvex_position = header;
        while let Some((kind, header, entry)) = try_read_box(child, &mut mvex_position) {
            if kind != TREX {
                continue;
            }
            trex_entries.push(read_trex(entry, header).ok_or("invalid trex")?);
        }
    }
    if video.0 == 0 || video.1 == 0 {
        return Err("video track was not found through hdlr=vide".into());
    }
    let trex = |id: u32| {
        trex_entries
            .iter()
            .find(|(track, _)| *track == id)
            .map(|(_, value)| *value)
            .unwrap_or_default()
    };
    let video = TrackIds {
        id: video.0,
        timescale: video.1,
        trex: trex(video.0),
    };
    let audio = if audio.0 == 0 {
        TrackIds::default()
    } else {
        if audio.1 == 0 {
            return Err("audio timescale is zero".into());
        }
        TrackIds {
            id: audio.0,
            timescale: audio.1,
            trex: trex(audio.0),
        }
    };
    Ok((video, audio))
}

fn read_track(trak: &[u8], header: usize) -> (u32, u32, u32) {
    let (mut track_id, mut timescale, mut handler) = (0, 0, 0);
    let mut position = header;
    while let Some((kind, child_header, child)) = try_read_box(trak, &mut position) {
        if kind == TKHD {
            track_id = versioned_u32(child, child_header);
            continue;
        }
        if kind != MDIA {
            continue;
        }
        let mut mdia_position = child_header;
        while let Some((kind, header, entry)) = try_read_box(child, &mut mdia_position) {
            match kind {
                MDHD => timescale = versioned_u32(entry, header),
                HDLR => {
                    handler = if entry.len() >= header + 12 {
                        be32(&entry[header + 8..])
                    } else {
                        0
                    }
                }
                _ => {}
            }
        }
    }
    (track_id, timescale, handler)
}

/// `readTkhdTrackID` / `readMdhdTimescale`: the field after the creation
/// and modification times, whose width depends on the box version.
fn versioned_u32(data: &[u8], header: usize) -> u32 {
    let offset = match data.get(header) {
        Some(1) => header + 20,
        Some(0) => header + 12,
        _ => return 0,
    };
    if data.len() >= offset + 4 {
        be32(&data[offset..])
    } else {
        0
    }
}

fn read_trex(data: &[u8], header: usize) -> Option<(u32, Trex)> {
    if data.len() < header + 24 {
        return None;
    }
    let track_id = be32(&data[header + 4..]);
    let description_index = be32(&data[header + 8..]);
    if track_id == 0 || description_index == 0 {
        return None;
    }
    Some((
        track_id,
        Trex {
            description_index,
            duration: be32(&data[header + 12..]),
            size: be32(&data[header + 16..]),
            flags: be32(&data[header + 20..]),
        },
    ))
}

fn find_box(data: &[u8], wanted: u32) -> Option<(&[u8], usize)> {
    let mut position = 0;
    while position < data.len() {
        let (kind, header, found) = try_read_box(data, &mut position)?;
        if kind == wanted {
            return Some((found, header));
        }
    }
    None
}

/// `tryReadBox`: the box at `position` (type, header size, whole box).
fn try_read_box<'a>(data: &'a [u8], position: &mut usize) -> Option<(u32, usize, &'a [u8])> {
    let start = *position;
    if start > data.len() || data.len() - start < 8 {
        return None;
    }
    let size32 = be32(&data[start..]);
    let kind = be32(&data[start + 4..]);
    let (size, header) = match size32 {
        1 => {
            if data.len() - start < 16 {
                return None;
            }
            (be64(&data[start + 8..]), 16)
        }
        0 => ((data.len() - start) as u64, 8),
        size => (u64::from(size), 8),
    };
    if size < header as u64 || size > i32::MAX as u64 || size > (data.len() - start) as u64 {
        return None;
    }
    let end = start + size as usize;
    *position = end;
    Some((kind, header, &data[start..end]))
}

fn read_u32(data: &[u8], position: &mut usize) -> Option<u32> {
    if data.len() < *position + 4 {
        return None;
    }
    let value = be32(&data[*position..]);
    *position += 4;
    Some(value)
}

fn to_units(seconds: f64, timescale: u32) -> Result<u64, Error> {
    let value = seconds * f64::from(timescale);
    if !value.is_finite() || value < 0.0 || value > u64::MAX as f64 {
        return Err(other("invalid timeline value"));
    }
    Ok(value.ceil() as u64)
}

fn timeline_nanoseconds(value: u64, timescale: u32) -> Result<u64, Error> {
    if timescale == 0 {
        return Err(other("timeline timescale is zero"));
    }
    let timescale = u64::from(timescale);
    let seconds = value / timescale;
    let remainder = value % timescale;
    let nanoseconds = seconds
        .checked_mul(1_000_000_000)
        .ok_or_else(|| other("timeline nanoseconds overflow"))?;
    nanoseconds
        .checked_add(remainder * 1_000_000_000 / timescale)
        .ok_or_else(|| other("timeline nanoseconds overflow"))
}

fn add_tfdt_offset(value: u64, timescale: u32, seconds: f64) -> Result<u64, Error> {
    if seconds <= 0.0 {
        return Ok(value);
    }
    let units = seconds * f64::from(timescale);
    if !units.is_finite() || units < 0.0 || units > u64::MAX as f64 {
        return Err(other("invalid tfdt offset"));
    }
    value
        .checked_add(units.round() as u64)
        .ok_or_else(|| other("tfdt offset overflow"))
}

fn write_tfdt(output: &mut Vec<u8>, decode_time: u64) {
    output.extend_from_slice(&20u32.to_be_bytes());
    output.extend_from_slice(&TFDT.to_be_bytes());
    output.extend_from_slice(&0x0100_0000u32.to_be_bytes());
    output.extend_from_slice(&decode_time.to_be_bytes());
}

fn write_mfhd(output: &mut Vec<u8>, sequence: u32) {
    output.extend_from_slice(&16u32.to_be_bytes());
    output.extend_from_slice(&MFHD.to_be_bytes());
    output.extend_from_slice(&0u32.to_be_bytes());
    output.extend_from_slice(&sequence.to_be_bytes());
}

fn write_header(output: &mut Vec<u8>, size: u32, kind: u32) {
    output.extend_from_slice(&size.to_be_bytes());
    output.extend_from_slice(&kind.to_be_bytes());
}

fn write_mdat_header(output: &mut Vec<u8>, payload_length: u64, header_size: usize) {
    if header_size == 8 {
        write_header(output, (payload_length + 8) as u32, MDAT);
        return;
    }
    write_header(output, 1, MDAT);
    output.extend_from_slice(&(payload_length + 16).to_be_bytes());
}

/// `scaledGreaterOrEqual`: `left/leftScale >= right/rightScale` with the
/// scales swapped across, in 128 bits.
fn scaled_greater_or_equal(left: u64, left_scale: u32, right: u64, right_scale: u32) -> bool {
    u128::from(left) * u128::from(left_scale) >= u128::from(right) * u128::from(right_scale)
}

fn four_cc_text(kind: u32) -> String {
    String::from_utf8_lossy(&kind.to_be_bytes()).into_owned()
}

fn be32(data: &[u8]) -> u32 {
    u32::from_be_bytes(data[..4].try_into().expect("four bytes"))
}

fn be64(data: &[u8]) -> u64 {
    u64::from_be_bytes(data[..8].try_into().expect("eight bytes"))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn mp4_box(kind: &[u8; 4], parts: &[&[u8]]) -> Vec<u8> {
        let body: Vec<u8> = parts.concat();
        [&((body.len() + 8) as u32).to_be_bytes()[..], kind, &body].concat()
    }

    fn full_box(kind: &[u8; 4], version_flags: u32, body: &[u8]) -> Vec<u8> {
        mp4_box(kind, &[&version_flags.to_be_bytes(), body])
    }

    fn words(values: &[u32]) -> Vec<u8> {
        values
            .iter()
            .flat_map(|value| value.to_be_bytes())
            .collect()
    }

    fn trak(id: u32, timescale: u32, handler: &[u8; 4]) -> Vec<u8> {
        let tkhd = full_box(b"tkhd", 0, &words(&[0, 0, id, 0, 0]));
        let mdhd = full_box(b"mdhd", 0, &words(&[0, 0, timescale, 0, 0]));
        let hdlr = full_box(b"hdlr", 0, &[&[0; 4][..], handler, &[0; 12]].concat());
        mp4_box(b"trak", &[&tkhd, &mp4_box(b"mdia", &[&mdhd, &hdlr])])
    }

    /// ftyp+moov as mp4mux writes it for one H.264 and one AAC track.
    pub(crate) fn init() -> Vec<u8> {
        let mvex = mp4_box(
            b"mvex",
            &[
                &full_box(b"trex", 0, &words(&[1, 1, 0, 0, 0])),
                &full_box(b"trex", 0, &words(&[2, 1, 0, 0, 0])),
            ],
        );
        [
            mp4_box(b"ftyp", &[b"iso6"]),
            mp4_box(
                b"moov",
                &[&trak(1, 1000, b"vide"), &trak(2, 8000, b"soun"), &mvex],
            ),
        ]
        .concat()
    }

    /// One moof+mdat: a traf with tfhd, tfdt and a trun of `samples`
    /// (duration, size, sync).
    pub(crate) fn fragment(
        track: u32,
        sequence: u32,
        decode_time: u64,
        samples: &[(u32, u32, bool)],
        fill: u8,
    ) -> Vec<u8> {
        let tfhd = full_box(b"tfhd", TFHD_DEFAULT_BASE_IS_MOOF, &words(&[track]));
        let tfdt = full_box(b"tfdt", 0x0100_0000, &decode_time.to_be_bytes());
        let entries: Vec<u32> = samples
            .iter()
            .flat_map(|(duration, size, sync)| {
                [
                    *duration,
                    *size,
                    if *sync { 0x0200_0000 } else { 0x0101_0000 },
                ]
            })
            .collect();
        let trun_body_len = 8 + entries.len() * 4;
        let traf_len = 8 + tfhd.len() + tfdt.len() + 12 + trun_body_len;
        let moof_len = 8 + 16 + traf_len;
        let trun = full_box(
            b"trun",
            TRUN_DATA_OFFSET | TRUN_DURATION | TRUN_SIZE | TRUN_FLAGS,
            &[
                &(samples.len() as u32).to_be_bytes()[..],
                &((moof_len + 8) as u32).to_be_bytes(),
                &words(&entries),
            ]
            .concat(),
        );
        let traf = mp4_box(b"traf", &[&tfhd, &tfdt, &trun]);
        let moof = mp4_box(
            b"moof",
            &[&full_box(b"mfhd", 0, &sequence.to_be_bytes()), &traf],
        );
        assert_eq!(moof.len(), moof_len);
        let size: u32 = samples.iter().map(|sample| sample.1).sum();
        [moof, mp4_box(b"mdat", &[&vec![fill; size as usize]])].concat()
    }

    fn segments(reader: &mut Reader, stream: &[u8], chunk: usize) -> Vec<Segment> {
        let mut result = Vec::new();
        for part in stream.chunks(chunk) {
            reader.push(part).unwrap();
            if let Some(segment) = reader.take_segment() {
                result.push(segment);
            }
            while reader.try_process_deferred().unwrap() {
                result.extend(reader.take_segment());
            }
        }
        result
    }

    /// Two-second video fragments (a key frame every two seconds) and
    /// one-second audio fragments; six-second segments.
    fn stream() -> Vec<u8> {
        let mut stream = init();
        let mut sequence = 1;
        for second in 0..8u64 {
            if second % 2 == 0 {
                stream.extend(fragment(
                    1,
                    sequence,
                    second * 1000,
                    &[(1000, 10, true), (1000, 10, false)],
                    b'v',
                ));
                sequence += 1;
            }
            stream.extend(fragment(
                2,
                sequence,
                second * 8000,
                &[(4000, 4, true), (4000, 4, true)],
                b'a',
            ));
            sequence += 1;
        }
        stream
    }

    #[test]
    fn fragments_merge_into_segments_at_key_frames() {
        let stream = stream();
        let mut reader = Reader::new(6.0, 0, false);
        let segments = segments(&mut reader, &stream, 7);
        assert_eq!(reader.take_init(), Some(init()));
        assert_eq!(segments.len(), 1);
        let segment = &segments[0];
        assert_eq!((segment.start_ns, segment.end_ns), (0, 6_000_000_000));
        // moof(mfhd, video traf with three 2-sample runs, audio traf) + mdat.
        let data = &segment.data;
        assert_eq!(&data[4..8], b"moof");
        let moof_size = be32(data) as usize;
        assert_eq!(&data[moof_size + 4..moof_size + 8], b"mdat");
        let payload = &data[moof_size + 8..];
        assert_eq!(payload.len(), 60 + 6 * 8);
        assert!(payload[..60].iter().all(|byte| *byte == b'v'));
        assert!(payload[60..].iter().all(|byte| *byte == b'a'));
        // The first video trun points just past the mdat header.
        let first_trun = data
            .windows(4)
            .position(|window| window == b"trun")
            .unwrap()
            - 4;
        assert_eq!(be32(&data[first_trun + 16..]) as usize, moof_size + 8);
    }

    #[test]
    fn the_rest_becomes_a_segment_at_end_of_stream() {
        let stream = stream();
        let mut reader = Reader::new(6.0, 0, false);
        let first = segments(&mut reader, &stream, stream.len());
        assert_eq!(first.len(), 1);
        assert!(!reader.try_process_deferred().unwrap());
        assert!(reader.try_build_end_of_stream_remainder().unwrap());
        let last = reader.take_segment().unwrap();
        assert_eq!((last.start_ns, last.end_ns), (6_000_000_000, 8_000_000_000));
        assert!(reader.end_of_stream_error().is_none());
        assert!(!reader.try_build_end_of_stream_remainder().unwrap());
    }

    #[test]
    fn the_init_segment_is_reported_once() {
        let stream = stream();
        let mut reader = Reader::new(6.0, 0, false);
        reader.push(&stream[..init().len() + 8]).unwrap();
        assert_eq!(reader.take_init().unwrap(), init());
        assert!(reader.take_init().is_none());
    }

    #[test]
    fn a_seek_offsets_the_timeline() {
        let stream = stream();
        let mut reader = Reader::new(6.0, 0, false);
        reader.seek_reset(12.0);
        let segments = segments(&mut reader, &stream, 64);
        assert_eq!(segments[0].start_ns, 12_000_000_000);
        // tfdt of the video traf carries the offset in the track timescale.
        let data = &segments[0].data;
        let tfdt = data
            .windows(4)
            .position(|window| window == b"tfdt")
            .unwrap()
            - 4;
        assert_eq!(be64(&data[tfdt + 12..]), 12_000);
    }

    /// The header length stays set until the box completes, so a stream cut
    /// inside a box body reports the header, as the reference does.
    #[test]
    fn a_truncated_stream_is_reported() {
        let stream = stream();
        let mut reader = Reader::new(6.0, 0, false);
        reader.push(&stream[..init().len() + 4]).unwrap();
        assert_eq!(
            reader.end_of_stream_error().unwrap().to_string(),
            "truncated mp4 fragment at end of stream: incomplete top-level box header: have=4, need=8"
        );
        reader
            .push(&stream[init().len() + 4..init().len() + 20])
            .unwrap();
        assert_eq!(
            reader.end_of_stream_error().unwrap().to_string(),
            "truncated mp4 fragment at end of stream: incomplete top-level box header: have=8, need=8"
        );
    }
}
