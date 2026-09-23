//! `server/gstreamer/probe.go`: what the module knows about a file, parsed
//! from the text `gst-discoverer-1.0 -v` prints.

use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;

use crate::{Config, Error};

static DURATION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)Duration:\s*(\d+):(\d+):(\d+)(?:\.(\d+))?").expect("a valid pattern")
});
static CONTAINER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:container(?:\s+#\d+)?|container[\s-]+format)\s*:\s*(.+)$")
        .expect("a valid pattern")
});
static STREAM: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(video|audio|subtitle|subtitles)(?:\s+#(\d+))?:\s*(.+)$")
        .expect("a valid pattern")
});
static INTEGER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"-?\d+").expect("a valid pattern"));
static RATE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\d+)\s*/\s*(\d+)").expect("a valid pattern"));

/// `ProbeInfo`; `/gst/:hash/probe` answers with it, so the JSON keys are the
/// Go field names.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ProbeInfo {
    #[serde(rename = "DurationNS")]
    pub duration_ns: i64,
    #[serde(rename = "FileSize")]
    pub file_size: i64,
    #[serde(rename = "Container")]
    pub container: String,
    #[serde(rename = "ContainerCapsName")]
    pub container_caps_name: String,
    /// `null` when discoverer found no stream, as Go writes a nil slice.
    #[serde(rename = "Tracks")]
    pub tracks: Option<Vec<TrackInfo>>,
}

/// `TrackInfo`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct TrackInfo {
    #[serde(rename = "Index")]
    pub index: i64,
    #[serde(rename = "PadName")]
    pub pad_name: String,
    #[serde(rename = "Type")]
    pub kind: String,
    #[serde(rename = "Codec")]
    pub codec: String,
    #[serde(rename = "CapsName")]
    pub caps_name: String,
    #[serde(rename = "Title")]
    pub title: String,
    #[serde(rename = "Language")]
    pub language: String,
    #[serde(rename = "Width")]
    pub width: i64,
    #[serde(rename = "Height")]
    pub height: i64,
    #[serde(rename = "Channels")]
    pub channels: i64,
    #[serde(rename = "Rate")]
    pub rate: i64,
    #[serde(rename = "FrameRateNum")]
    pub frame_rate_num: i64,
    #[serde(rename = "FrameRateDen")]
    pub frame_rate_den: i64,
    #[serde(rename = "Colorimetry")]
    pub colorimetry: String,
    #[serde(rename = "Transfer")]
    pub transfer: String,
    #[serde(rename = "Primaries")]
    pub primaries: String,
    #[serde(rename = "Matrix")]
    pub matrix: String,
    #[serde(rename = "BitDepth")]
    pub bit_depth: i64,
    #[serde(rename = "HasMasteringDisplayInfo")]
    pub has_mastering_display_info: bool,
    #[serde(rename = "HasContentLightLevel")]
    pub has_content_light_level: bool,
    #[serde(rename = "IsDolbyVision")]
    pub is_dolby_vision: bool,
    #[serde(rename = "DolbyVisionProfile")]
    pub dolby_vision_profile: i64,
    #[serde(rename = "VideoTransfer")]
    pub video_transfer: String,
}

const VIDEO_CAPS: [&str; 5] = [
    "video/x-h264",
    "video/x-h265",
    "video/x-av1",
    "video/x-vp9",
    "video/x-vp8",
];

impl ProbeInfo {
    pub fn tracks(&self) -> &[TrackInfo] {
        self.tracks.as_deref().unwrap_or_default()
    }

    pub fn duration_seconds(&self) -> i64 {
        if self.duration_ns <= 0 {
            0
        } else {
            self.duration_ns / 1_000_000_000
        }
    }

    pub fn video(&self) -> Option<&TrackInfo> {
        self.tracks()
            .iter()
            .find(|track| track.kind == "video" || VIDEO_CAPS.contains(&track.caps_name.as_str()))
    }

    fn video_caps_name(&self) -> &str {
        self.video().map_or("", |video| video.caps_name.as_str())
    }

    pub fn audio(&self) -> Option<&TrackInfo> {
        self.tracks().iter().find(|track| track.kind == "audio")
    }

    /// The audio track with this index, or the first one.
    pub fn audio_track(&self, index: i64) -> Option<&TrackInfo> {
        let mut audio = self.tracks().iter().filter(|track| track.kind == "audio");
        let first = audio.clone().next();
        audio.find(|track| track.index == index).or(first)
    }

    pub fn has_audio(&self) -> bool {
        self.audio().is_some()
    }

    fn container_text(&self) -> String {
        format!("{} {}", self.container, self.container_caps_name)
            .trim()
            .to_lowercase()
    }

    pub fn is_matroska_container(&self) -> bool {
        let container = self.container_text();
        container.contains("matroska") || container.contains("webm")
    }

    pub fn is_avi_container(&self) -> bool {
        let container = self.container_text();
        container.contains("video/x-msvideo") || container.contains("avi")
    }

    pub fn is_h264(&self) -> bool {
        self.video_caps_name() == "video/x-h264"
    }
    pub fn is_h265(&self) -> bool {
        self.video_caps_name() == "video/x-h265"
    }
    pub fn is_av1(&self) -> bool {
        self.video_caps_name() == "video/x-av1"
    }
    pub fn is_vp9(&self) -> bool {
        self.video_caps_name() == "video/x-vp9"
    }
    pub fn is_vp8(&self) -> bool {
        self.video_caps_name() == "video/x-vp8"
    }

    /// `validateProbe`: what the HLS pipeline accepts.
    pub fn validate(&self, config: &Config) -> Result<(), Error> {
        let Some(video) = self.video() else {
            return Err(Error::ProbeUnavailable);
        };
        if config.hdr_to_sdr
            && video.is_hdr_video()
            && video.video_transfer != "pq"
            && video.video_transfer != "hlg"
        {
            return Err(Error::UnsupportedHdrTransfer);
        }
        let transcode_avi = self.is_avi_container() && config.transcode_avi;
        if !self.is_matroska_container() && !transcode_avi {
            let name = self.container.trim();
            return Err(Error::UnsupportedContainer(if name.is_empty() {
                "<unknown>".into()
            } else {
                name.into()
            }));
        }
        let supported = self.is_h264()
            || self.is_h265()
            || self.is_av1()
            || self.is_vp9()
            || (self.is_vp8() && config.transcode_vp8)
            || transcode_avi;
        if supported {
            Ok(())
        } else {
            Err(Error::UnsupportedVideo)
        }
    }
}

impl TrackInfo {
    pub fn is_aac_audio(&self) -> bool {
        if self.kind != "audio" {
            return false;
        }
        let codec = self.codec.to_lowercase();
        [
            "aac",
            "mp4a",
            "mpeg-4",
            "mpegversion=(int)4",
            "mpegversion=4",
        ]
        .iter()
        .any(|needle| codec.contains(needle))
    }

    pub fn is_hdr_video(&self) -> bool {
        self.kind == "video"
            && (self.is_dolby_vision || self.video_transfer == "pq" || self.video_transfer == "hlg")
    }
}

/// `probeFromDiscoverer`.
pub fn from_discoverer(text: &str) -> ProbeInfo {
    let mut probe = ProbeInfo {
        duration_ns: duration_ns(text),
        ..ProbeInfo::default()
    };
    let mut tracks: Vec<TrackInfo> = Vec::new();
    let mut current: Option<usize> = None;
    for raw in text.replace("\r\n", "\n").split('\n') {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(captures) = CONTAINER.captures(line) {
            let container = captures[1].trim();
            if probe.container.is_empty() && !container.is_empty() {
                probe.container = container.into();
                probe.container_caps_name = container_to_caps_name(container);
            }
            current = None;
            continue;
        }
        let caps = container_caps_from_line(line);
        if !caps.is_empty() {
            probe.container = caps.into();
            probe.container_caps_name = caps.into();
            current = None;
            continue;
        }
        if line.to_lowercase().starts_with("properties:") {
            current = None;
            continue;
        }
        if let Some(track) = stream_header(line) {
            tracks.push(track);
            current = Some(tracks.len() - 1);
            continue;
        }
        if let Some(index) = current {
            track_line(&mut tracks[index], line);
        }
    }

    let mut counters = [0i64; 3];
    for track in &mut tracks {
        let (slot, prefix) = match track.kind.as_str() {
            "video" => (0, "video_"),
            "audio" => (1, "audio_"),
            "subtitle" => (2, "subtitle_"),
            _ => continue,
        };
        track.index = counters[slot];
        track.pad_name = format!("{prefix}{}", counters[slot]);
        counters[slot] += 1;
    }
    if !tracks.is_empty() {
        probe.tracks = Some(tracks);
    }
    probe
}

fn stream_header(line: &str) -> Option<TrackInfo> {
    let captures = STREAM.captures(line)?;
    let mut kind = captures[1].to_lowercase();
    if kind == "subtitles" {
        kind = "subtitle".into();
    }
    let mut codec = captures[3].trim().to_string();
    if kind == "subtitle" {
        codec = subtitle_codec(&codec);
    }
    let caps_name = codec_to_caps_name(&kind, &codec);
    Some(TrackInfo {
        kind,
        codec,
        caps_name,
        ..TrackInfo::default()
    })
}

fn track_line(track: &mut TrackInfo, line: &str) {
    video_metadata(track, line);
    if starts_with_fold(line, "Width:") {
        track.width = int_after_colon(line);
    } else if starts_with_fold(line, "Height:") {
        track.height = int_after_colon(line);
    } else if starts_with_fold(line, "Channels:") {
        track.channels = int_after_colon(line);
    } else if starts_with_fold(line, "Sample rate:") {
        track.rate = int_after_colon(line);
    } else if starts_with_fold(line, "language code:") {
        track.language = value_after_colon(line);
    } else if starts_with_fold(line, "language name:") {
        if track.language.is_empty() {
            track.language = value_after_colon(line);
        }
    } else if starts_with_fold(line, "title:") {
        track.title = value_after_colon(line);
    } else if starts_with_fold(line, "audio codec:") || starts_with_fold(line, "video codec:") {
        if track.codec.is_empty() {
            track.codec = value_after_colon(line);
        }
        if track.caps_name.is_empty() {
            track.caps_name = codec_to_caps_name(&track.kind, &track.codec);
        }
    } else if starts_with_fold(line, "subtitle codec:") {
        track.codec = subtitle_codec(&value_after_colon(line));
        track.caps_name = codec_to_caps_name(&track.kind, &track.codec);
    } else if starts_with_fold(line, "Frame rate:") {
        (track.frame_rate_num, track.frame_rate_den) = rate(&value_after_colon(line));
    }
}

fn duration_ns(text: &str) -> i64 {
    let Some(captures) = DURATION.captures(text) else {
        return 0;
    };
    let number = |index: usize| captures[index].parse::<i64>().unwrap_or(0);
    let mut fraction = captures
        .get(4)
        .map_or("", |value| value.as_str())
        .to_string();
    fraction.truncate(9);
    while fraction.len() < 9 {
        fraction.push('0');
    }
    number(1)
        .wrapping_mul(3_600_000_000_000)
        .wrapping_add(number(2).wrapping_mul(60_000_000_000))
        .wrapping_add(number(3).wrapping_mul(1_000_000_000))
        .wrapping_add(fraction.parse::<i64>().unwrap_or(0))
}

fn codec_to_caps_name(kind: &str, codec: &str) -> String {
    let codec = codec.to_lowercase();
    if codec.is_empty() {
        return String::new();
    }
    let has = |needle: &str| codec.contains(needle);
    let caps = match kind {
        "video" if has("h264") || has("h.264") || has("avc") => "video/x-h264",
        "video" if has("hevc") || has("h265") || has("h.265") => "video/x-h265",
        "video" if has("av1") => "video/x-av1",
        "video" if has("vp9") => "video/x-vp9",
        "video" if has("vp8") => "video/x-vp8",
        "audio" if has("eac3") || has("e-ac-3") || has("e-ac3") => "audio/x-eac3",
        "audio" if has("ac3") || has("ac-3") || has("a/52") => "audio/x-ac3",
        "audio" if has("aac") => "audio/mpeg",
        "audio" if has("opus") => "audio/x-opus",
        "audio" if has("vorbis") => "audio/x-vorbis",
        "audio" if has("flac") => "audio/x-flac",
        "audio" if has("mpeg") || has("mp3") => "audio/mpeg",
        "subtitle" => match subtitle_codec(&codec).as_str() {
            "text" | "subrip" | "utf8" => "text/x-raw",
            "ass" => "application/x-ass",
            "ssa" => "application/x-ssa",
            "pgs" => "subpicture/x-pgs",
            "dvd" => "subpicture/x-dvd",
            "kate" => "subtitle/x-kate",
            _ => "application/x-subtitle-unknown",
        },
        _ => "",
    };
    caps.into()
}

fn subtitle_codec(value: &str) -> String {
    let codec = value.trim().to_lowercase();
    let has = |needle: &str| codec.contains(needle);
    let name = if has("subrip") || has("srt") {
        "subrip"
    } else if has("utf8") || has("utf-8") {
        "utf8"
    } else if has("ass") {
        "ass"
    } else if has("ssa") {
        "ssa"
    } else if has("pgs") {
        "pgs"
    } else if has("dvd") {
        "dvd"
    } else if has("kate") {
        "kate"
    } else if has("text") {
        "text"
    } else {
        return codec;
    };
    name.into()
}

fn video_metadata(track: &mut TrackInfo, line: &str) {
    if track.kind != "video" {
        return;
    }
    if track.colorimetry.is_empty() {
        track.colorimetry = metadata_field(line, "colorimetry");
    }
    if track.transfer.is_empty() {
        track.transfer = first_non_empty([
            metadata_field(line, "transfer"),
            metadata_field(line, "transfer-characteristics"),
            metadata_field(line, "transfer-function"),
        ]);
    }
    if track.primaries.is_empty() {
        track.primaries = first_non_empty([
            metadata_field(line, "primaries"),
            metadata_field(line, "color-primaries"),
        ]);
    }
    if track.matrix.is_empty() {
        track.matrix = first_non_empty([
            metadata_field(line, "matrix"),
            metadata_field(line, "matrix-coefficients"),
        ]);
    }
    if track.bit_depth == 0 {
        for name in ["bit-depth-luma", "bit-depth", "bits-per-component"] {
            let value = metadata_field(line, name);
            if !value.is_empty() {
                track.bit_depth = first_integer(&value).parse().unwrap_or(0);
                if track.bit_depth > 0 {
                    break;
                }
            }
        }
        let upper = line.to_uppercase();
        if track.bit_depth == 0
            && (upper.contains("P010") || upper.contains("10LE") || upper.contains("10BE"))
        {
            track.bit_depth = 10;
        } else if track.bit_depth == 0
            && (upper.contains("P012") || upper.contains("12LE") || upper.contains("12BE"))
        {
            track.bit_depth = 12;
        }
    }
    let lower = line.to_lowercase();
    track.has_mastering_display_info |= lower.contains("mastering-display-info");
    track.has_content_light_level |=
        lower.contains("content-light-level") || lower.contains("max-cll");
    track.is_dolby_vision |= lower.contains("dolby vision")
        || lower.contains("video/x-dolby-vision")
        || lower.contains("dovi");
    if track.dolby_vision_profile == 0 {
        let profile = metadata_field(line, "profile");
        if track.is_dolby_vision && !profile.is_empty() {
            track.dolby_vision_profile = first_integer(&profile).parse().unwrap_or(0);
        }
    }
    let transfer = format!("{} {} {line}", track.transfer, track.colorimetry).to_lowercase();
    if transfer.contains("smpte2084") || transfer.contains("st2084") || transfer.contains("pq") {
        track.video_transfer = "pq".into();
    } else if transfer.contains("arib-std-b67") || transfer.contains("hlg") {
        track.video_transfer = "hlg".into();
    }
}

/// `metadataField`: the value after `name` up to the next comma, with a
/// `(type)` prefix and quotes removed. The search is case-insensitive but
/// slices the original line, as Go does with the lowered index.
fn metadata_field(line: &str, name: &str) -> String {
    let lower = line.to_lowercase();
    let Some(index) = lower.find(&name.to_lowercase()) else {
        return String::new();
    };
    let Some(rest) = line.get(index + name.len()..) else {
        return String::new();
    };
    let mut rest = rest.trim().trim_start_matches([' ', '=', ':']);
    if rest.starts_with('(')
        && let Some(close) = rest.find(')')
    {
        rest = rest[close + 1..].trim();
    }
    if let Some(comma) = rest.find(',') {
        rest = &rest[..comma];
    }
    rest.trim().trim_matches(['"', '\'']).into()
}

fn first_integer(value: &str) -> &str {
    INTEGER.find(value).map_or("", |found| found.as_str())
}

fn first_non_empty<const N: usize>(values: [String; N]) -> String {
    values
        .into_iter()
        .find(|value| !value.is_empty())
        .unwrap_or_default()
}

fn container_to_caps_name(value: &str) -> String {
    let direct = container_caps_from_line(value);
    if !direct.is_empty() {
        return direct.into();
    }
    let lower = value.to_lowercase();
    let caps = if lower.contains("webm") {
        "video/webm"
    } else if lower.contains("matroska") {
        "video/x-matroska"
    } else if lower.contains("avi") {
        "video/x-msvideo"
    } else if lower.contains("quicktime") || lower.contains("mp4") {
        "video/quicktime"
    } else {
        ""
    };
    caps.into()
}

fn container_caps_from_line(value: &str) -> &'static str {
    let lower = value.trim().to_lowercase();
    let lower = lower.split(',').next().unwrap_or_default();
    let lower = lower.trim_matches(['"', '\'']);
    [
        "audio/x-matroska",
        "video/x-matroska",
        "video/x-matroska-3d",
        "audio/webm",
        "video/webm",
        "video/x-msvideo",
        "video/quicktime",
        "video/mp4",
        "video/mpegts",
        "application/ogg",
        "video/x-flv",
    ]
    .into_iter()
    .find(|caps| *caps == lower)
    .unwrap_or("")
}

fn rate(value: &str) -> (i64, i64) {
    let Some(captures) = RATE.captures(value) else {
        return (0, 0);
    };
    match (captures[1].parse::<i64>(), captures[2].parse::<i64>()) {
        (Ok(num), Ok(den)) if num > 0 && den > 0 => (num, den),
        _ => (0, 0),
    }
}

fn int_after_colon(line: &str) -> i64 {
    first_integer(&value_after_colon(line)).parse().unwrap_or(0)
}

fn value_after_colon(line: &str) -> String {
    let Some((_, value)) = line.split_once(':') else {
        return String::new();
    };
    let value = value.trim();
    if value == "<unknown>" {
        String::new()
    } else {
        value.into()
    }
}

fn starts_with_fold(value: &str, prefix: &str) -> bool {
    value
        .get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discoverer_output_becomes_tracks() {
        let probe = from_discoverer(
            "
Properties:
  Duration: 1:34:09.920000000
  Seekable: yes
  container: Matroska
    video #1: H.264 (High Profile)
      Width: 1920
      Height: 800
      Frame rate: 24000/1001
      language code: und
    audio #2: AC-3 (ATSC A/52)
      Sample rate: 48000
      Channels: 6
      language code: ru
      title: DUB
    audio #3: E-AC-3 (ATSC A/52B)
      Sample rate: 48000
      Channels: 6
      language code: en
      title: Original
",
        );
        assert_eq!(probe.duration_ns, 5_649_920_000_000);
        assert_eq!(probe.container, "Matroska");
        assert_eq!(probe.container_caps_name, "video/x-matroska");
        let tracks = probe.tracks();
        assert_eq!(tracks.len(), 3);
        assert_eq!(
            (
                tracks[0].kind.as_str(),
                tracks[0].index,
                tracks[0].pad_name.as_str()
            ),
            ("video", 0, "video_0")
        );
        assert_eq!(tracks[0].caps_name, "video/x-h264");
        assert_eq!(
            (
                tracks[0].width,
                tracks[0].height,
                tracks[0].frame_rate_num,
                tracks[0].frame_rate_den
            ),
            (1920, 800, 24000, 1001)
        );
        assert_eq!(
            (
                tracks[1].pad_name.as_str(),
                tracks[1].caps_name.as_str(),
                tracks[1].rate
            ),
            ("audio_0", "audio/x-ac3", 48000)
        );
        assert_eq!(
            (tracks[1].language.as_str(), tracks[1].title.as_str()),
            ("ru", "DUB")
        );
        assert_eq!(
            (tracks[2].pad_name.as_str(), tracks[2].caps_name.as_str()),
            ("audio_1", "audio/x-eac3")
        );
    }

    #[test]
    fn containers_and_codecs_are_classified() {
        let probe = from_discoverer(
            "Properties:\n  Duration: 0:01:00.000000000\n  container #0: Matroska\n    video #1: H.264\n    audio #2: MPEG-4 AAC\n",
        );
        assert!(probe.is_matroska_container());
        let audio = probe.audio_track(0).unwrap();
        assert_eq!(
            (audio.codec.as_str(), audio.caps_name.as_str()),
            ("MPEG-4 AAC", "audio/mpeg")
        );
        assert!(audio.is_aac_audio());
        let webm = from_discoverer("container #0: WebM\n    video #1: VP9\n");
        assert!(webm.is_matroska_container());
        assert!(!webm.has_audio());
        assert!(
            TrackInfo {
                kind: "audio".into(),
                codec: "audio/mpeg, mpegversion=(int)4, stream-format=(string)raw".into(),
                ..TrackInfo::default()
            }
            .is_aac_audio()
        );
    }

    #[test]
    fn validation_accepts_only_what_the_pipeline_handles() {
        let h264 = || {
            Some(vec![TrackInfo {
                kind: "video".into(),
                caps_name: "video/x-h264".into(),
                ..TrackInfo::default()
            }])
        };
        let quicktime = ProbeInfo {
            container: "Quicktime".into(),
            tracks: h264(),
            ..ProbeInfo::default()
        };
        assert_eq!(
            quicktime
                .validate(&Config::default())
                .unwrap_err()
                .to_string(),
            "unsupported container; only Matroska/WebM is supported: Quicktime"
        );
        let avi = ProbeInfo {
            container: "AVI".into(),
            container_caps_name: "video/x-msvideo".into(),
            tracks: h264(),
            ..ProbeInfo::default()
        };
        assert!(avi.validate(&Config::default()).is_err());
        let transcode_avi = Config {
            transcode_avi: true,
            ..Config::default()
        };
        assert!(avi.validate(&transcode_avi).is_ok());
        let vp8 = ProbeInfo {
            container: "WebM".into(),
            tracks: Some(vec![TrackInfo {
                kind: "video".into(),
                caps_name: "video/x-vp8".into(),
                ..TrackInfo::default()
            }]),
            ..ProbeInfo::default()
        };
        assert_eq!(
            vp8.validate(&Config::default()).unwrap_err().to_string(),
            "unsupported video codec"
        );
        assert!(
            vp8.validate(&Config {
                transcode_vp8: true,
                ..Config::default()
            })
            .is_ok()
        );
        assert_eq!(
            ProbeInfo::default()
                .validate(&Config::default())
                .unwrap_err()
                .to_string(),
            "gst-discoverer returned no stream info"
        );
    }

    #[test]
    fn video_metadata_detects_hdr() {
        let probe = from_discoverer(
            "container: Matroska\n  video #0: H.265 (Main 10 Profile)\n    Tags: colorimetry=bt2100-pq, bit-depth-luma=(uint)10\n",
        );
        let video = probe.video().unwrap();
        assert_eq!(video.colorimetry, "bt2100-pq");
        assert_eq!(video.bit_depth, 10);
        assert_eq!(video.video_transfer, "pq");
        assert!(video.is_hdr_video());
    }
}
