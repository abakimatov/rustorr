//! `server/gstreamer/subtitles.go`: WebVTT cues collected from the
//! pipeline's `webvttenc` branches and cut into per-segment files.

use std::{
    collections::HashSet,
    sync::{LazyLock, Mutex},
    time::Duration,
};

use regex::Regex;
use tokio::sync::watch;

use crate::probe::TrackInfo;

const MAX_PENDING_CHARS: usize = 64 * 1024;
/// How long a subtitle request waits for the video to reach its segment.
pub const WAIT_TIMEOUT: Duration = Duration::from_secs(5);

static CUE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?ms)(?:^|\n)(?:[^\n]*\n)?(\d{2,}:\d{2}:\d{2}[.,]\d{3})[ \t]+-->[ \t]+(\d{2,}:\d{2}:\d{2}[.,]\d{3})[^\n]*\n(.*?)(?:\n[ \t]*\n)",
    )
    .expect("a valid pattern")
});

/// `supportedSubtitleTrack`: text formats `webvttenc` accepts.
pub fn supported_track(track: &TrackInfo) -> bool {
    track.kind == "subtitle"
        && matches!(
            track.codec.to_lowercase().as_str(),
            "text" | "subrip" | "utf8" | "ass" | "ssa"
        )
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Cue {
    start_ns: u64,
    end_ns: u64,
    text: String,
}

#[derive(Default)]
struct Inner {
    pending: String,
    video_read_to: u64,
    cues: Vec<Cue>,
    seen: HashSet<Cue>,
}

/// `subtitleStore`: one track's cues and how far the video has been read.
/// Every change bumps the watch channel, which is what waiting requests
/// listen to.
pub struct Store {
    inner: Mutex<Inner>,
    updated: watch::Sender<u64>,
}

impl Default for Store {
    fn default() -> Self {
        Self {
            inner: Mutex::default(),
            updated: watch::channel(0).0,
        }
    }
}

impl Store {
    pub fn notify(&self) {
        self.updated.send_modify(|version| *version += 1);
    }

    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.updated.subscribe()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn set_video_read_to(&self, value: u64) {
        let changed = {
            let mut inner = self.lock();
            let changed = inner.video_read_to != value;
            inner.video_read_to = value;
            changed
        };
        if changed {
            self.notify();
        }
    }

    pub fn advance_video_read_to(&self, value: u64) {
        if value == 0 {
            return;
        }
        let changed = {
            let mut inner = self.lock();
            let changed = value > inner.video_read_to;
            if changed {
                inner.video_read_to = value;
            }
            changed
        };
        if changed {
            self.notify();
        }
    }

    /// `appendVTT`: complete cues in `chunk` (after what was left over last
    /// time) are recorded once each. After a seek the encoder restarts its
    /// clock at zero, so cues that would lie before the seek point by more
    /// than a segment are shifted by it.
    pub fn append_vtt(&self, chunk: &str, seek_seconds: f64, max_back_diff_ns: u64) {
        if chunk.trim().is_empty() {
            return;
        }
        let chunk = chunk.replace("\r\n", "\n").replace('\r', "\n");
        {
            let mut inner = self.lock();
            let vtt = std::mem::take(&mut inner.pending) + &chunk;
            let mut consumed = 0;
            for captures in CUE.captures_iter(&vtt) {
                consumed = captures.get(0).map_or(consumed, |whole| whole.end());
                let (Some(mut start_ns), Some(mut end_ns)) =
                    (parse_clock(&captures[1]), parse_clock(&captures[2]))
                else {
                    continue;
                };
                let text = captures[3].trim();
                if end_ns <= start_ns || text.is_empty() {
                    continue;
                }
                let seek_ns = if seek_seconds > 0.0 {
                    (seek_seconds * 1_000_000_000.0) as u64
                } else {
                    0
                };
                let minimum = seek_ns.saturating_sub(max_back_diff_ns);
                if seek_ns > 0 && start_ns < minimum {
                    start_ns = start_ns.saturating_add(seek_ns);
                    end_ns = end_ns.saturating_add(seek_ns);
                }
                let cue = Cue {
                    start_ns,
                    end_ns,
                    text: text.into(),
                };
                if inner.seen.insert(cue.clone()) {
                    inner.cues.push(cue);
                }
            }
            let mut pending = vtt[consumed..].to_string();
            if pending.len() > MAX_PENDING_CHARS {
                let mut cut = pending.len() - MAX_PENDING_CHARS;
                while !pending.is_char_boundary(cut) {
                    cut += 1;
                }
                if let Some(newline) = pending[cut..].find('\n') {
                    cut += newline + 1;
                }
                pending = pending[cut..].to_string();
            }
            inner.pending = pending;
        }
        self.notify();
    }

    /// `renderVTT`: the cues overlapping `[from, to)`, relative to `from`,
    /// and whether the video has been read far enough for the list to be
    /// complete.
    pub fn render(&self, from_ns: u64, to_ns: u64) -> (String, bool) {
        let inner = self.lock();
        let mut result = header(from_ns);
        for cue in &inner.cues {
            if cue.end_ns <= from_ns || cue.start_ns >= to_ns {
                continue;
            }
            let start = cue.start_ns.saturating_sub(from_ns);
            let end = if cue.end_ns < to_ns {
                cue.end_ns - from_ns
            } else {
                to_ns - from_ns
            };
            if end <= start {
                continue;
            }
            result.push_str(&format!(
                "{} --> {}\n{}\n\n",
                format_clock(start),
                format_clock(end),
                cue.text
            ));
        }
        let ready = to_ns <= from_ns || inner.video_read_to >= to_ns;
        (result, ready)
    }
}

/// `emptyVTT` / `writeVTTHeader`: the timestamp map ties the file to the
/// segment's position on the MPEG-TS clock.
pub fn header(start_ns: u64) -> String {
    format!(
        "WEBVTT\nX-TIMESTAMP-MAP=LOCAL:00:00:00.000,MPEGTS:{}\n\n",
        start_ns / 1_000_000_000 * 90_000 + start_ns % 1_000_000_000 * 90_000 / 1_000_000_000
    )
}

fn parse_clock(value: &str) -> Option<u64> {
    let value = value.replacen(',', ".", 1);
    let parts: Vec<&str> = value.trim().split(':').collect();
    let [hours, minutes, seconds] = parts[..] else {
        return None;
    };
    let hours: u64 = hours.parse().ok()?;
    let minutes: u64 = minutes.parse().ok()?;
    let (seconds, milliseconds) = seconds.split_once('.')?;
    if minutes > 59 || milliseconds.contains('.') {
        return None;
    }
    let seconds: u64 = seconds.parse().ok()?;
    if seconds > 59 {
        return None;
    }
    let mut milliseconds: String = milliseconds.chars().take(3).collect();
    while milliseconds.len() < 3 {
        milliseconds.push('0');
    }
    let milliseconds: u64 = milliseconds.parse().ok()?;
    Some((hours * 3600 + minutes * 60 + seconds) * 1_000_000_000 + milliseconds * 1_000_000)
}

fn format_clock(value_ns: u64) -> String {
    let total_ms = value_ns / 1_000_000;
    let total_seconds = total_ms / 1000;
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        total_seconds / 3600,
        total_seconds / 60 % 60,
        total_seconds % 60,
        total_ms % 1000
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_cues_are_kept_once_and_rendered_per_segment() {
        let store = Store::default();
        store.append_vtt(
            "WEBVTT\n\n00:00:01.000 --> 00:00:03.000\nHello\n\n00:00:07.500 --> 00:00:08",
            0.0,
            6_000_000_000,
        );
        store.append_vtt(".000\nLater\n\n", 0.0, 6_000_000_000);
        store.append_vtt(
            "00:00:01.000 --> 00:00:03.000\nHello\n\n",
            0.0,
            6_000_000_000,
        );
        let (first, ready) = store.render(0, 6_000_000_000);
        assert_eq!(
            first,
            "WEBVTT\nX-TIMESTAMP-MAP=LOCAL:00:00:00.000,MPEGTS:0\n\n00:00:01.000 --> 00:00:03.000\nHello\n\n"
        );
        assert!(!ready);
        store.advance_video_read_to(12_000_000_000);
        let (second, ready) = store.render(6_000_000_000, 12_000_000_000);
        assert_eq!(
            second,
            "WEBVTT\nX-TIMESTAMP-MAP=LOCAL:00:00:00.000,MPEGTS:540000\n\n00:00:01.500 --> 00:00:02.000\nLater\n\n"
        );
        assert!(ready);
    }

    #[test]
    fn cues_restarted_after_a_seek_are_shifted() {
        let store = Store::default();
        store.append_vtt(
            "00:00:01.000 --> 00:00:02.000\nAfter seek\n\n",
            60.0,
            6_000_000_000,
        );
        let (vtt, _) = store.render(60_000_000_000, 66_000_000_000);
        assert!(
            vtt.ends_with("00:00:01.000 --> 00:00:02.000\nAfter seek\n\n"),
            "{vtt}"
        );
    }

    #[test]
    fn clocks_parse_like_the_reference() {
        assert_eq!(parse_clock("01:02:03,45"), Some(3_723_450_000_000));
        assert_eq!(parse_clock("00:60:00.000"), None);
        assert_eq!(format_clock(3_723_450_000_000), "01:02:03.450");
    }
}
