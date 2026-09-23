//! `server/gstreamer/handlers.go`: the HLS playlists.

use crate::{
    Config, cue::CueTimeline, mp4init::VariantInfo, probe::ProbeInfo, subtitles::supported_track,
};

/// What the playlists are built from: a task's fixed facts and, once the
/// init segment exists, what it says about the variant.
pub struct Media<'a> {
    pub id: &'a str,
    pub audio: i64,
    pub config: &'a Config,
    pub probe: &'a ProbeInfo,
    pub cue: Option<&'a CueTimeline>,
    pub variant: Option<&'a VariantInfo>,
}

/// `videoIsTranscoded`.
pub fn video_is_transcoded(config: &Config, probe: &ProbeInfo) -> bool {
    if config.hdr_to_sdr && probe.video().is_some_and(|video| video.is_hdr_video()) {
        return true;
    }
    if config.transcode_avi && probe.is_avi_container() {
        return true;
    }
    if probe.is_h264() {
        config.transcode_h264
    } else if probe.is_h265() {
        config.transcode_h265
    } else if probe.is_av1() {
        config.transcode_av1
    } else if probe.is_vp9() {
        config.transcode_vp9
    } else if probe.is_vp8() {
        config.transcode_vp8
    } else {
        false
    }
}

/// `effectiveAACChannels`.
pub fn aac_channels(config: &Config, channels: Option<i64>) -> i64 {
    let mut value = config.aac_channels;
    if value <= 0 {
        value = channels.unwrap_or(0);
    }
    if value <= 0 {
        value = 2;
    }
    value.clamp(1, 8)
}

/// `buildVariantPlaylist`: the master playlist.
pub fn master(media: &Media<'_>, seconds: i64) -> String {
    let mut playlist = String::from("#EXTM3U\n#EXT-X-VERSION:7\n\n");
    let id = path_escape(media.id);
    let mut has_subtitles = false;
    if media.config.subtitles {
        for track in media
            .probe
            .tracks()
            .iter()
            .filter(|track| supported_track(track))
        {
            has_subtitles = true;
            let language = quoted(if track.language.is_empty() {
                "und"
            } else {
                &track.language
            });
            let name = if track.title.is_empty() {
                format!("Subtitle {}", track.index)
            } else {
                track.title.clone()
            };
            playlist.push_str(&format!(
                "#EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID=\"subs\",NAME=\"{}\",LANGUAGE=\"{language}\",DEFAULT=NO,AUTOSELECT=YES,FORCED=NO,URI=\"/gst/{id}/subs/{}.m3u8\"\n",
                quoted(&name),
                track.index
            ));
        }
        if has_subtitles {
            playlist.push('\n');
        }
    }

    let (bandwidth, average) = bandwidth(media);
    playlist.push_str(&format!("#EXT-X-STREAM-INF:BANDWIDTH={bandwidth}"));
    if average > 0 {
        playlist.push_str(&format!(",AVERAGE-BANDWIDTH={average}"));
    }
    if let Some(video) = media.probe.video() {
        let (mut width, mut height) = (video.width, video.height);
        let mut frame_rate = 0.0;
        let mut codecs = codecs(media);
        let mut range = video.video_transfer.to_uppercase();
        if let Some(variant) = media.variant {
            if variant.width > 0 {
                width = variant.width;
            }
            if variant.height > 0 {
                height = variant.height;
            }
            frame_rate = variant.frame_rate;
            if !variant.codecs.is_empty() {
                codecs = variant.codecs.clone();
            }
            if !variant.video_range.is_empty() {
                range = variant.video_range.to_uppercase();
            }
        }
        if width > 0 && height > 0 {
            playlist.push_str(&format!(",RESOLUTION={width}x{height}"));
        }
        if frame_rate <= 0.0 && video.frame_rate_num > 0 && video.frame_rate_den > 0 {
            frame_rate = video.frame_rate_num as f64 / video.frame_rate_den as f64;
        }
        if frame_rate > 0.0 {
            playlist.push_str(&format!(
                ",FRAME-RATE={}",
                trim_decimal(&format!("{frame_rate:.3}"))
            ));
        }
        if !codecs.is_empty() {
            playlist.push_str(&format!(",CODECS=\"{codecs}\""));
        }
        if media.config.hdr_to_sdr && video.is_hdr_video() {
            range = "SDR".into();
        }
        if matches!(range.as_str(), "PQ" | "HLG" | "SDR") {
            playlist.push_str(&format!(",VIDEO-RANGE={range}"));
        }
    }
    if has_subtitles {
        playlist.push_str(",SUBTITLES=\"subs\"");
    }
    playlist.push_str(&format!("\n/gst/{id}/video.m3u8?audio={}", media.audio));
    if seconds > 0 {
        playlist.push_str(&format!("&seconds={seconds}"));
    }
    playlist.push('\n');
    playlist
}

/// `hlsBandwidth`: peak and average bits per second.
fn bandwidth(media: &Media<'_>) -> (i64, i64) {
    let config = media.config;
    let mut audio = config.aac_bitrate_kbps.max(1) * 1000;
    let channels = media
        .probe
        .audio_track(media.audio)
        .map(|track| track.channels);
    if aac_channels(config, channels) > 2 {
        audio *= 2;
    }
    if video_is_transcoded(config, media.probe) {
        let average = config.video_bitrate.max(1) * 1000 + audio;
        return (average * 125 / 100, average);
    }
    if media.probe.file_size > 0 && media.probe.duration_ns > 0 {
        let seconds = media.probe.duration_ns as f64 / 1_000_000_000.0;
        let value = media.probe.file_size as f64 * 8.0 / seconds;
        if value.is_finite() && value > 0.0 && value <= i64::MAX as f64 / 2.0 {
            let average = value.ceil() as i64;
            return (average * 150 / 100, average);
        }
    }
    let fallback = config.video_bitrate.max(1) * 1000 + audio;
    ((fallback * 150 / 100).max(4_000_000), 0)
}

/// `hlsCodecs`: a guess from the probe until the init segment is known.
fn codecs(media: &Media<'_>) -> String {
    let probe = media.probe;
    let video = if video_is_transcoded(media.config, probe) || probe.is_h264() {
        "avc1.4d401f"
    } else if probe.is_h265() {
        "hvc1"
    } else if probe.is_av1() {
        "av01.0.08M.08"
    } else if probe.is_vp9() {
        "vp09.00.10.08"
    } else {
        ""
    };
    if probe.has_audio() {
        format!("{video},mp4a.40.2").trim_matches(',').into()
    } else {
        video.into()
    }
}

/// `hlsQuoted`: control characters become spaces, `\` and `"` are escaped.
fn quoted(value: &str) -> String {
    value
        .chars()
        .map(|char| {
            if char < '\u{20}' || char == '\u{7f}' {
                ' '
            } else {
                char
            }
        })
        .collect::<String>()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

/// Segment count and target duration of the media playlist.
pub fn segment_count(media: &Media<'_>) -> usize {
    if let Some(cue) = media.cue {
        return cue.segments.len();
    }
    let segment_ns = media.config.segment_seconds.max(1) * 1_000_000_000;
    let duration = effective_duration_ns(media.probe);
    (1 + (duration - 1) / segment_ns) as usize
}

fn effective_duration_ns(probe: &ProbeInfo) -> i64 {
    if probe.duration_ns > 0 {
        probe.duration_ns
    } else {
        200 * 60 * 1_000_000_000
    }
}

/// `segmentStartNS`.
pub fn segment_start_ns(config: &Config, cue: Option<&CueTimeline>, index: i64) -> u64 {
    if let Some(segment) = cue.and_then(|cue| cue.segment(index)) {
        return segment.start_ns;
    }
    if index <= 0 {
        return 0;
    }
    index as u64 * config.segment_seconds.max(1) as u64 * 1_000_000_000
}

/// `buildTaskPlaylist`: the media playlist from `start_index`.
pub fn media_playlist(media: &Media<'_>, start_index: i64, audio: i64) -> String {
    let segment_seconds = media.config.segment_seconds.max(1);
    let duration_ns = effective_duration_ns(media.probe);
    let count = segment_count(media) as i64;
    let target_duration = media.cue.map_or(segment_seconds, |cue| {
        cue.max_duration_ns.div_ceil(1_000_000_000) as i64
    });
    let start = start_index.clamp(0, count);
    let mut playlist = format!(
        "#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:{}\n#EXT-X-MEDIA-SEQUENCE:{start}\n#EXT-X-MAP:URI=\"init.mp4?audio={audio}",
        target_duration.max(1)
    );
    if start > 0 {
        playlist.push_str(&format!(
            "&seconds={}",
            segment_start_ns(media.config, media.cue, start) / 1_000_000_000
        ));
    }
    playlist.push_str("\"\n");
    let segment_ns = segment_seconds as u64 * 1_000_000_000;
    for index in start..count {
        let mut item_ns = segment_ns;
        if let Some(segment) = media.cue.and_then(|cue| cue.segment(index)) {
            item_ns = segment.duration_ns();
        } else if index == count - 1 {
            let start_ns = index as u64 * segment_ns;
            if duration_ns as u64 > start_ns {
                item_ns = duration_ns as u64 - start_ns;
            }
        }
        playlist.push_str(&format!(
            "#EXTINF:{},\nseg/{index}.m4s\n",
            trim_decimal(&format!("{:.6}", item_ns as f64 / 1_000_000_000.0))
        ));
    }
    playlist.push_str("#EXT-X-ENDLIST\n");
    playlist
}

/// `buildSubtitlePlaylist`: the media playlist with WebVTT files instead of
/// segments and no init map.
pub fn subtitle_playlist(media: &Media<'_>, track: i64) -> String {
    let mut result = String::new();
    for line in media_playlist(media, 0, media.audio).split('\n') {
        if line.starts_with("#EXT-X-MAP:") {
            continue;
        }
        if let Some(index) = line.strip_prefix("seg/") {
            let index = index.strip_suffix(".m4s").unwrap_or(index);
            result.push_str(&format!("{track}/{index}.vtt\n"));
        } else {
            result.push_str(line);
            result.push('\n');
        }
    }
    result
}

/// `strings.TrimRight(strings.TrimRight(s, "0"), ".")`.
fn trim_decimal(value: &str) -> &str {
    value.trim_end_matches('0').trim_end_matches('.')
}

/// `url.PathEscape`.
fn path_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric()
            || matches!(
                byte,
                b'-' | b'_' | b'.' | b'~' | b'$' | b'&' | b'+' | b':' | b'=' | b'@'
            )
        {
            escaped.push(byte as char);
        } else {
            escaped.push_str(&format!("%{byte:02X}"));
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{cue::CueSegment, probe::from_discoverer};

    fn probe() -> ProbeInfo {
        let mut probe = from_discoverer(
            "Duration: 0:00:08.000000000\ncontainer #0: Matroska\n  video #1: H.264 (Constrained Baseline Profile)\n    Width: 64\n    Height: 48\n    Frame rate: 10/1\n  audio #2: Raw 16-bit PCM audio\n    Sample rate: 8000\n    Channels: 1\n    language code: ru\n  subtitles #3: UTF-8 plain text\n    language code: en\n    title: English\n",
        );
        probe.file_size = 500_844;
        probe
    }

    fn cue() -> CueTimeline {
        CueTimeline {
            segments: [0, 2, 4, 6]
                .map(|start| CueSegment {
                    start_ns: start * 1_000_000_000,
                    end_ns: (start + 2) * 1_000_000_000,
                })
                .to_vec(),
            timestamp_scale_ns: 1_000_000,
            max_duration_ns: 2_000_000_000,
        }
    }

    #[test]
    fn master_lists_subtitles_and_the_variant() {
        let config = Config::platform_defaults();
        let probe = probe();
        let media = Media {
            id: "abc",
            audio: 0,
            config: &config,
            probe: &probe,
            cue: None,
            variant: None,
        };
        assert_eq!(
            master(&media, 0),
            "#EXTM3U\n#EXT-X-VERSION:7\n\n#EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID=\"subs\",NAME=\"English\",LANGUAGE=\"en\",DEFAULT=NO,AUTOSELECT=YES,FORCED=NO,URI=\"/gst/abc/subs/0.m3u8\"\n\n#EXT-X-STREAM-INF:BANDWIDTH=751266,AVERAGE-BANDWIDTH=500844,RESOLUTION=64x48,FRAME-RATE=10,CODECS=\"avc1.4d401f,mp4a.40.2\",SUBTITLES=\"subs\"\n/gst/abc/video.m3u8?audio=0\n"
        );
        let variant = VariantInfo {
            codecs: "avc1.42C00A,mp4a.40.2".into(),
            video_range: "SDR".into(),
            width: 64,
            height: 48,
            frame_rate: 10.0,
        };
        let media = Media {
            variant: Some(&variant),
            ..media
        };
        assert!(master(&media, 12).ends_with(
            "FRAME-RATE=10,CODECS=\"avc1.42C00A,mp4a.40.2\",VIDEO-RANGE=SDR,SUBTITLES=\"subs\"\n/gst/abc/video.m3u8?audio=0&seconds=12\n"
        ));
    }

    #[test]
    fn media_playlists_follow_the_cue_timeline_or_fixed_segments() {
        let config = Config::platform_defaults();
        let probe = probe();
        let timeline = cue();
        let fixed = Media {
            id: "abc",
            audio: 0,
            config: &config,
            probe: &probe,
            cue: None,
            variant: None,
        };
        assert_eq!(
            media_playlist(&fixed, 0, 0),
            "#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:6\n#EXT-X-MEDIA-SEQUENCE:0\n#EXT-X-MAP:URI=\"init.mp4?audio=0\"\n#EXTINF:6,\nseg/0.m4s\n#EXTINF:2,\nseg/1.m4s\n#EXT-X-ENDLIST\n"
        );
        let cued = Media {
            cue: Some(&timeline),
            ..fixed
        };
        assert_eq!(
            media_playlist(&cued, 2, 0),
            "#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:2\n#EXT-X-MAP:URI=\"init.mp4?audio=0&seconds=4\"\n#EXTINF:2,\nseg/2.m4s\n#EXTINF:2,\nseg/3.m4s\n#EXT-X-ENDLIST\n"
        );
        assert_eq!(
            subtitle_playlist(&cued, 0),
            "#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:0\n#EXTINF:2,\n0/0.vtt\n#EXTINF:2,\n0/1.vtt\n#EXTINF:2,\n0/2.vtt\n#EXTINF:2,\n0/3.vtt\n#EXT-X-ENDLIST\n\n"
        );
    }

    #[test]
    fn quoting_and_escaping_match_go() {
        assert_eq!(quoted("a\"b\\c\td"), "a\\\"b\\\\c d");
        assert_eq!(path_escape("a b/ä"), "a%20b%2F%C3%A4");
        assert_eq!(trim_decimal("23.976"), "23.976");
        assert_eq!(trim_decimal("25.000"), "25");
    }
}
