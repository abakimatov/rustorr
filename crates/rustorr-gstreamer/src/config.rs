//! `server/gstreamer/config.go`: the module's settings, stored apart from
//! the BitTorrent settings.

use serde::{Deserialize, Serialize};

/// The oldest GStreamer the module claims to support.
pub const MIN_GST_VERSION: f64 = 1.22;

/// Field for field and in the order of the reference's `Config`, which is
/// also its JSON form.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    #[serde(rename = "GSTVersion")]
    pub gst_version: f64,
    #[serde(rename = "GSTPath")]
    pub gst_path: String,
    #[serde(rename = "Source")]
    pub source: String,
    #[serde(rename = "MaxTasks")]
    pub max_tasks: i64,
    #[serde(rename = "InactiveMinutes")]
    pub inactive_minutes: i64,
    #[serde(rename = "AACBitrateKbps")]
    pub aac_bitrate_kbps: i64,
    #[serde(rename = "AACChannels")]
    pub aac_channels: i64,
    #[serde(rename = "AACSamplerate")]
    pub aac_samplerate: i64,
    #[serde(rename = "SegmentSeconds")]
    pub segment_seconds: i64,
    #[serde(rename = "SegmentDiff")]
    pub segment_diff: i64,
    #[serde(rename = "Subtitles")]
    pub subtitles: bool,
    #[serde(rename = "TranscodeH264")]
    pub transcode_h264: bool,
    #[serde(rename = "TranscodeH265")]
    pub transcode_h265: bool,
    #[serde(rename = "TranscodeAV1")]
    pub transcode_av1: bool,
    #[serde(rename = "TranscodeVP9")]
    pub transcode_vp9: bool,
    #[serde(rename = "TranscodeVP8")]
    pub transcode_vp8: bool,
    #[serde(rename = "TranscodeAVI")]
    pub transcode_avi: bool,
    #[serde(rename = "HDRToSDR")]
    pub hdr_to_sdr: bool,
    #[serde(rename = "HardwareAcceleration")]
    pub hardware_acceleration: bool,
    #[serde(rename = "UseGPU")]
    pub use_gpu: bool,
    #[serde(rename = "X264Ultrafast")]
    pub x264_ultrafast: bool,
    #[serde(rename = "VideoBitrate")]
    pub video_bitrate: i64,
}

/// A Go zero value: what the reference decodes into before applying a
/// request or a stored document.
impl Default for Config {
    fn default() -> Self {
        Self {
            gst_version: 0.0,
            gst_path: String::new(),
            source: String::new(),
            max_tasks: 0,
            inactive_minutes: 0,
            aac_bitrate_kbps: 0,
            aac_channels: 0,
            aac_samplerate: 0,
            segment_seconds: 0,
            segment_diff: 0,
            subtitles: false,
            transcode_h264: false,
            transcode_h265: false,
            transcode_av1: false,
            transcode_vp9: false,
            transcode_vp8: false,
            transcode_avi: false,
            hdr_to_sdr: false,
            hardware_acceleration: false,
            use_gpu: false,
            x264_ultrafast: false,
            video_bitrate: 0,
        }
    }
}

impl Config {
    /// `defaultConfigWithoutSettings` on Linux.
    pub fn platform_defaults() -> Self {
        Self {
            gst_version: MIN_GST_VERSION,
            source: "stream".into(),
            inactive_minutes: 5,
            aac_bitrate_kbps: 256,
            segment_seconds: 6,
            segment_diff: 20,
            subtitles: true,
            hardware_acceleration: true,
            use_gpu: true,
            video_bitrate: 10_000,
            ..Self::default()
        }
    }

    /// `Config.normalized`.
    pub fn normalized(mut self) -> Self {
        if self.inactive_minutes <= 0 {
            self.inactive_minutes = 5;
        }
        if self.aac_bitrate_kbps <= 0 {
            self.aac_bitrate_kbps = 256;
        }
        self.aac_channels = self.aac_channels.max(0);
        self.aac_samplerate = self.aac_samplerate.max(0);
        self.max_tasks = self.max_tasks.max(0);
        if self.segment_seconds <= 0 {
            self.segment_seconds = 6;
        }
        self.segment_diff = self.segment_diff.max(0);
        if self.video_bitrate <= 0 {
            self.video_bitrate = 10_000;
        }
        if self.gst_version < MIN_GST_VERSION {
            self.gst_version = MIN_GST_VERSION;
        }
        self.source = self.source.trim().to_lowercase();
        if self.source != "play" {
            self.source = "stream".into();
        }
        self
    }

    /// `applySettingsConfig`: fields present in a stored document override
    /// the defaults one by one; a document that does not parse is ignored.
    pub fn with_stored(mut self, document: &str) -> Self {
        let Ok(serde_json::Value::Object(stored)) = serde_json::from_str(document) else {
            return self;
        };
        let mut merged = match serde_json::to_value(&self) {
            Ok(serde_json::Value::Object(fields)) => fields,
            _ => return self,
        };
        for (key, value) in stored {
            // encoding/json matches field names case-insensitively.
            let Some(name) = merged
                .keys()
                .find(|name| name.eq_ignore_ascii_case(&key))
                .cloned()
            else {
                continue;
            };
            if !value.is_null() {
                merged.insert(name, value);
            }
        }
        if let Ok(config) = serde_json::from_value(serde_json::Value::Object(merged)) {
            self = config;
        }
        self
    }

    /// The document `SaveConfig` stores: the normalized config.
    pub fn stored_document(&self) -> String {
        serde_json::to_string(&self.clone().normalized()).expect("a config serialises")
    }

    pub(crate) fn uses_play_source(&self) -> bool {
        self.clone().normalized().source == "play"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_serialise_like_the_reference() {
        assert_eq!(
            serde_json::to_string(&Config::platform_defaults().normalized()).unwrap(),
            r#"{"GSTVersion":1.22,"GSTPath":"","Source":"stream","MaxTasks":0,"InactiveMinutes":5,"AACBitrateKbps":256,"AACChannels":0,"AACSamplerate":0,"SegmentSeconds":6,"SegmentDiff":20,"Subtitles":true,"TranscodeH264":false,"TranscodeH265":false,"TranscodeAV1":false,"TranscodeVP9":false,"TranscodeVP8":false,"TranscodeAVI":false,"HDRToSDR":false,"HardwareAcceleration":true,"UseGPU":true,"X264Ultrafast":false,"VideoBitrate":10000}"#
        );
    }

    #[test]
    fn normalization_repairs_out_of_range_values() {
        let config = Config {
            source: " PLAY ".into(),
            max_tasks: -1,
            segment_diff: -3,
            gst_version: 1.0,
            ..Config::default()
        }
        .normalized();
        assert_eq!(config.source, "play");
        assert_eq!(config.max_tasks, 0);
        assert_eq!(config.segment_diff, 0);
        assert_eq!(config.segment_seconds, 6);
        assert_eq!(config.gst_version, MIN_GST_VERSION);
        assert_eq!(
            Config {
                source: "files".into(),
                ..Config::default()
            }
            .normalized()
            .source,
            "stream"
        );
    }

    #[test]
    fn stored_fields_override_defaults_one_by_one() {
        let config = Config::platform_defaults()
            .with_stored(r#"{"segmentseconds":4,"Subtitles":false,"Unknown":1,"GSTPath":null}"#);
        assert_eq!(config.segment_seconds, 4);
        assert!(!config.subtitles);
        assert_eq!(config.aac_bitrate_kbps, 256);
        assert_eq!(
            Config::platform_defaults().with_stored("not json"),
            Config::platform_defaults()
        );
    }
}
