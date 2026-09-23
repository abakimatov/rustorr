//! `/ffp/status` and `/ffp/:hash/:id`: MatriX.145's media inspection. The
//! reference runs `ffprobe` on its own `/play` URL through
//! `vansante/go-ffprobe` v2.3.1 and re-encodes the result from that
//! library's Go structs, so the answer has their fields, order and
//! `omitempty` rules rather than ffprobe's raw output.

use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use axum::{
    body::Body,
    extract::{Path as UrlPath, State},
    http::{HeaderMap, Response, StatusCode, header},
    response::IntoResponse,
};
use serde_json::{Map, Value};
use tokio::{io::AsyncReadExt, process::Command};

use crate::{
    ApiError,
    app::{AppState, management_authorized, unauthorized},
};

/// go-ffprobe's context: the probe is cancelled after a minute.
const TIMEOUT: Duration = Duration::from_secs(60);

/// Where `ffprobe` is: `PATH` first, then next to the executable, as the
/// reference's `init` looks; otherwise the bare name, which only exists
/// relative to the working directory.
pub fn locate_ffprobe() -> PathBuf {
    let in_path = std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|directory| directory.join("ffprobe"))
            .find(|candidate| is_executable(candidate))
    });
    in_path
        .or_else(|| {
            let beside = std::env::current_exe().ok()?.parent()?.join("ffprobe");
            beside.exists().then_some(beside)
        })
        .unwrap_or_else(|| PathBuf::from("ffprobe"))
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

/// `ffprobe.Exists`.
fn available(binary: &Path) -> bool {
    binary.exists()
}

pub(crate) async fn status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response<Body>, Response<Body>> {
    if !management_authorized(&state, &headers) {
        return Err(unauthorized());
    }
    let available = available(&state.http.ffprobe);
    crate::app::json_response(serde_json::json!({ "available": available }))
        .map_err(IntoResponse::into_response)
}

pub(crate) async fn probe(
    State(state): State<AppState>,
    UrlPath((hash, id)): UrlPath<(String, String)>,
    headers: HeaderMap,
) -> Result<Response<Body>, Response<Body>> {
    if !management_authorized(&state, &headers) {
        return Err(unauthorized());
    }
    let error =
        |status: StatusCode, message: String| ApiError::Json { status, message }.into_response();
    if !available(&state.http.ffprobe) {
        return Err(error(
            StatusCode::NOT_FOUND,
            "ffprobe binary not found".into(),
        ));
    }
    let link = format!("http://127.0.0.1:{}/play/{hash}/{id}", state.http.port);
    match run(&state.http.ffprobe, &link).await {
        Ok(json) => Ok((
            [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
            json,
        )
            .into_response()),
        Err(message) => Err(error(
            StatusCode::BAD_REQUEST,
            format!("error getting data: {message}"),
        )),
    }
}

/// `ProbeURL` and `runProbe`: the re-encoded probe data or go-ffprobe's
/// error text.
async fn run(binary: &Path, url: &str) -> Result<String, String> {
    let shown = binary.display().to_string();
    let mut child = Command::new(binary)
        .args([
            "-loglevel",
            "fatal",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
            "-show_chapters",
            url,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("error running {shown} [] {error}"))?;
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let collect = async {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let _ = tokio::join!(stdout.read_to_end(&mut out), stderr.read_to_end(&mut err));
        let status = child.wait().await;
        (out, err, status)
    };
    let (out, err, status) = match tokio::time::timeout(TIMEOUT, collect).await {
        Ok(result) => result,
        Err(_) => return Err(format!("error running {shown} [] signal: killed")),
    };
    let stderr = String::from_utf8_lossy(&err);
    match status {
        Ok(status) if status.success() => {}
        Ok(status) => {
            use std::os::unix::process::ExitStatusExt;
            let reason = match (status.code(), status.signal()) {
                (Some(code), _) => format!("exit status {code}"),
                (None, Some(signal)) => format!("signal: {}", signal_name(signal)),
                _ => "exit status 1".into(),
            };
            return Err(format!("error running {shown} [{stderr}] {reason}"));
        }
        Err(error) => return Err(format!("error running {shown} [{stderr}] {error}")),
    }
    let parsed: Value = serde_json::from_slice(&out)
        .map_err(|error| format!("error parsing ffprobe output: {error}"))?;
    let Some(object) = parsed.as_object() else {
        return Err("error parsing ffprobe output: json: cannot unmarshal into Go value of type ffprobe.ProbeData".into());
    };
    if !object.get("format").is_some_and(Value::is_object) {
        return Err("no format data found in ffprobe output".into());
    }
    Ok(probe_data(object).render())
}

fn signal_name(signal: i32) -> &'static str {
    match signal {
        9 => "killed",
        15 => "terminated",
        6 => "aborted",
        11 => "segmentation fault",
        _ => "signal",
    }
}

/// A JSON value written in a fixed field order, as Go encodes structs.
enum Json {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    /// A float inside a JSON string (`json:",string"`).
    FloatString(f64),
    Str(String),
    Array(Vec<Json>),
    Object(Vec<(&'static str, Json)>),
    /// A Go map: keys sorted.
    Map(Vec<(String, Json)>),
}

impl Json {
    fn render(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }

    fn write(&self, out: &mut String) {
        match self {
            Self::Null => out.push_str("null"),
            Self::Bool(value) => out.push_str(if *value { "true" } else { "false" }),
            Self::Int(value) => out.push_str(&value.to_string()),
            Self::Float(value) => out.push_str(&go_float(*value)),
            Self::FloatString(value) => {
                out.push('"');
                out.push_str(&go_float(*value));
                out.push('"');
            }
            Self::Str(text) => out.push_str(&String::from_utf8_lossy(
                &crate::error::go_json(&text).expect("a string encodes"),
            )),
            Self::Array(items) => {
                out.push('[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    item.write(out);
                }
                out.push(']');
            }
            Self::Object(fields) => {
                write_fields(out, fields.iter().map(|(key, value)| (*key, value)))
            }
            Self::Map(fields) => {
                write_fields(out, fields.iter().map(|(key, value)| (key.as_str(), value)))
            }
        }
    }
}

fn write_fields<'a>(out: &mut String, fields: impl Iterator<Item = (&'a str, &'a Json)>) {
    out.push('{');
    for (index, (key, value)) in fields.enumerate() {
        if index > 0 {
            out.push(',');
        }
        Json::Str(key.to_owned()).write(out);
        out.push(':');
        value.write(out);
    }
    out.push('}');
}

/// Go's `encoding/json` float: shortest form, exponents only outside
/// `[1e-6, 1e21)`, written without a leading zero in the exponent.
fn go_float(value: f64) -> String {
    let magnitude = value.abs();
    if magnitude != 0.0 && !(1e-6..1e21).contains(&magnitude) {
        let text = format!("{value:e}");
        let (mantissa, exponent) = text.split_once('e').unwrap_or((&text, "0"));
        let (sign, digits) = match exponent.strip_prefix('-') {
            Some(digits) => ('-', digits),
            None => ('+', exponent),
        };
        let digits = if digits.len() < 2 {
            format!("0{digits}")
        } else {
            digits.to_owned()
        };
        return format!("{mantissa}e{sign}{digits}");
    }
    format!("{value}")
}

fn string(object: &Map<String, Value>, key: &str) -> Json {
    Json::Str(
        object
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
    )
}

fn int(object: &Map<String, Value>, key: &str) -> i64 {
    object.get(key).and_then(Value::as_i64).unwrap_or_default()
}

/// A `json:",string"` float field: a string holding a number.
fn string_float(object: &Map<String, Value>, key: &str) -> Json {
    Json::FloatString(
        object
            .get(key)
            .and_then(Value::as_str)
            .and_then(|text| text.parse().ok())
            .unwrap_or_default(),
    )
}

/// `Tags`, a Go map of whatever values ffprobe wrote.
fn tags(object: &Map<String, Value>, key: &str) -> Json {
    match object.get(key) {
        Some(Value::Object(map)) => any(&Value::Object(map.clone())),
        _ => Json::Null,
    }
}

/// An `interface{}` value, re-encoded as Go does.
fn any(value: &Value) -> Json {
    match value {
        Value::Null => Json::Null,
        Value::Bool(flag) => Json::Bool(*flag),
        Value::Number(number) => Json::Float(number.as_f64().unwrap_or_default()),
        Value::String(text) => Json::Str(text.clone()),
        Value::Array(items) => Json::Array(items.iter().map(any).collect()),
        Value::Object(map) => {
            let mut fields: Vec<(String, Json)> = map
                .iter()
                .map(|(key, value)| (key.clone(), any(value)))
                .collect();
            fields.sort_by(|left, right| left.0.cmp(&right.0));
            Json::Map(fields)
        }
    }
}

fn optional_int(
    fields: &mut Vec<(&'static str, Json)>,
    object: &Map<String, Value>,
    key: &'static str,
) {
    let value = int(object, key);
    if value != 0 {
        fields.push((key, Json::Int(value)));
    }
}

fn optional_string(
    fields: &mut Vec<(&'static str, Json)>,
    object: &Map<String, Value>,
    key: &'static str,
) {
    if let Some(text) = object
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
    {
        fields.push((key, Json::Str(text.into())));
    }
}

fn probe_data(object: &Map<String, Value>) -> Json {
    let list = |key: &str, item: fn(&Map<String, Value>) -> Json| match object.get(key) {
        Some(Value::Array(items)) => Json::Array(
            items
                .iter()
                .map(|value| value.as_object().map_or(Json::Null, item))
                .collect(),
        ),
        _ => Json::Null,
    };
    Json::Object(vec![
        ("streams", list("streams", stream)),
        (
            "format",
            object
                .get("format")
                .and_then(Value::as_object)
                .map_or(Json::Null, format),
        ),
        ("chapters", list("chapters", chapter)),
    ])
}

fn format(object: &Map<String, Value>) -> Json {
    Json::Object(vec![
        ("filename", string(object, "filename")),
        ("nb_streams", Json::Int(int(object, "nb_streams"))),
        ("nb_programs", Json::Int(int(object, "nb_programs"))),
        ("format_name", string(object, "format_name")),
        ("format_long_name", string(object, "format_long_name")),
        ("start_time", string_float(object, "start_time")),
        ("duration", string_float(object, "duration")),
        ("size", string(object, "size")),
        ("bit_rate", string(object, "bit_rate")),
        ("probe_score", Json::Int(int(object, "probe_score"))),
        ("tags", tags(object, "tags")),
    ])
}

fn chapter(object: &Map<String, Value>) -> Json {
    Json::Object(vec![
        ("id", Json::Int(int(object, "id"))),
        ("time_base", string(object, "time_base")),
        ("start_time", string_float(object, "start_time")),
        ("end_time", string_float(object, "end_time")),
        ("tags", tags(object, "tags")),
    ])
}

const DISPOSITION: [&str; 11] = [
    "default",
    "dub",
    "original",
    "comment",
    "lyrics",
    "karaoke",
    "forced",
    "hearing_impaired",
    "visual_impaired",
    "clean_effects",
    "attached_pic",
];

fn stream(object: &Map<String, Value>) -> Json {
    let disposition = object.get("disposition").and_then(Value::as_object);
    let disposition = Json::Object(
        DISPOSITION
            .iter()
            .map(|key| (*key, Json::Int(disposition.map_or(0, |map| int(map, key)))))
            .collect(),
    );
    let mut fields = vec![
        ("index", Json::Int(int(object, "index"))),
        ("id", string(object, "id")),
        ("codec_name", string(object, "codec_name")),
        ("codec_long_name", string(object, "codec_long_name")),
        ("codec_type", string(object, "codec_type")),
        ("codec_time_base", string(object, "codec_time_base")),
        ("codec_tag_string", string(object, "codec_tag_string")),
        ("codec_tag", string(object, "codec_tag")),
        ("r_frame_rate", string(object, "r_frame_rate")),
        ("avg_frame_rate", string(object, "avg_frame_rate")),
        ("time_base", string(object, "time_base")),
        ("start_pts", Json::Int(int(object, "start_pts"))),
        ("start_time", string(object, "start_time")),
        (
            "duration_ts",
            Json::Int(
                object
                    .get("duration_ts")
                    .and_then(Value::as_i64)
                    .unwrap_or_default(),
            ),
        ),
        ("duration", string(object, "duration")),
        ("bit_rate", string(object, "bit_rate")),
        ("bits_per_raw_sample", string(object, "bits_per_raw_sample")),
        ("nb_frames", string(object, "nb_frames")),
        ("disposition", disposition),
        ("tags", tags(object, "tags")),
    ];
    optional_string(&mut fields, object, "field_order");
    optional_string(&mut fields, object, "profile");
    fields.push(("width", Json::Int(int(object, "width"))));
    fields.push(("height", Json::Int(int(object, "height"))));
    optional_int(&mut fields, object, "has_b_frames");
    for key in ["sample_aspect_ratio", "display_aspect_ratio", "pix_fmt"] {
        optional_string(&mut fields, object, key);
    }
    optional_int(&mut fields, object, "level");
    for key in [
        "color_range",
        "color_space",
        "color_transfer",
        "color_primaries",
        "sample_fmt",
        "sample_rate",
    ] {
        optional_string(&mut fields, object, key);
    }
    optional_int(&mut fields, object, "channels");
    optional_string(&mut fields, object, "channel_layout");
    optional_int(&mut fields, object, "bits_per_sample");
    if let Some(Value::Array(list)) = object.get("side_data_list")
        && !list.is_empty()
    {
        fields.push((
            "side_data_list",
            Json::Array(list.iter().map(side_data).collect()),
        ));
    }
    Json::Object(fields)
}

/// `SideData.MarshalJSON`: the typed struct for known side data, the raw
/// map for anything else.
fn side_data(value: &Value) -> Json {
    let Some(object) = value.as_object() else {
        return any(value);
    };
    let kind = object
        .get("side_data_type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let base = ("side_data_type", Json::Str(kind.into()));
    let flex_float = |key: &str| -> Option<f64> {
        match object.get(key)? {
            Value::Number(number) => number.as_f64(),
            Value::String(text) => match text.split_once('/') {
                Some((numerator, denominator)) => {
                    let (numerator, denominator): (f64, f64) =
                        (numerator.parse().ok()?, denominator.parse().ok()?);
                    (denominator != 0.0).then(|| numerator / denominator)
                }
                None => text.parse().ok(),
            },
            _ => None,
        }
    };
    match kind {
        "Display Matrix" => Json::Object(vec![
            base,
            ("displaymatrix", string(object, "displaymatrix")),
            ("rotation", Json::Int(int(object, "rotation"))),
        ]),
        "Stereo 3D" => Json::Object(vec![
            base,
            ("type", string(object, "type")),
            (
                "inverted",
                Json::Bool(match object.get("inverted") {
                    Some(Value::Bool(flag)) => *flag,
                    Some(Value::Number(number)) => number.as_i64() == Some(1),
                    Some(Value::String(text)) => matches!(text.as_str(), "true" | "yes" | "1"),
                    _ => false,
                }),
            ),
        ]),
        "Spherical Mapping" => {
            let mut fields = vec![base, ("projection", string(object, "projection"))];
            for key in [
                "padding",
                "bound_left",
                "bound_top",
                "bound_right",
                "bound_bottom",
                "yaw",
                "pitch",
                "roll",
            ] {
                optional_int(&mut fields, object, key);
            }
            Json::Object(fields)
        }
        "Skip Samples" => Json::Object(vec![
            base,
            ("skip_samples", Json::Int(int(object, "skip_samples"))),
            ("discard_padding", Json::Int(int(object, "discard_padding"))),
            ("skip_reason", Json::Int(int(object, "skip_reason"))),
            ("discard_reason", Json::Int(int(object, "discard_reason"))),
        ]),
        "Mastering display metadata" => {
            let mut fields = vec![base];
            for key in [
                "red_x",
                "red_y",
                "green_x",
                "green_y",
                "blue_x",
                "blue_y",
                "white_point_x",
                "white_point_y",
                "min_luminance",
                "max_luminance",
            ] {
                if let Some(value) = flex_float(key).filter(|value| *value != 0.0) {
                    fields.push((key, Json::Float(value)));
                }
            }
            Json::Object(fields)
        }
        "Content light level metadata" => {
            let mut fields = vec![base];
            optional_int(&mut fields, object, "max_content");
            optional_int(&mut fields, object, "max_average");
            Json::Object(fields)
        }
        _ => any(value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floats_format_like_go() {
        assert_eq!(go_float(2.0), "2");
        assert_eq!(go_float(0.02322), "0.02322");
        assert_eq!(go_float(3600.5), "3600.5");
        assert_eq!(go_float(1e-7), "1e-07");
        assert_eq!(go_float(0.0), "0");
    }

    #[test]
    fn probe_output_is_re_encoded_through_the_go_structs() {
        // The reference's answer for the WAV fixture, from its raw ffprobe
        // output.
        let raw: Value = serde_json::from_str(r#"{
            "streams": [{"index": 0, "codec_name": "pcm_s16le", "codec_long_name": "PCM signed 16-bit little-endian",
              "codec_type": "audio", "codec_tag_string": "[1][0][0][0]", "codec_tag": "0x0001", "sample_fmt": "s16",
              "sample_rate": "8000", "channels": 1, "bits_per_sample": 16, "initial_padding": 0, "r_frame_rate": "0/0",
              "avg_frame_rate": "0/0", "time_base": "1/8000", "duration_ts": 16000, "duration": "2.000000", "bit_rate": "128000",
              "disposition": {"default": 0, "dub": 0, "original": 0, "comment": 0, "lyrics": 0, "karaoke": 0,
                "forced": 0, "hearing_impaired": 0, "visual_impaired": 0, "clean_effects": 0, "attached_pic": 0,
                "timed_thumbnails": 0}}],
            "chapters": [],
            "format": {"filename": "http://127.0.0.1:8090/play/x/1", "nb_streams": 1, "nb_programs": 0,
              "format_name": "wav", "format_long_name": "WAV / WAVE (Waveform Audio)", "duration": "2.000000", "size": "32044", "bit_rate": "128176", "probe_score": 99}
        }"#).unwrap();
        let rendered = probe_data(raw.as_object().unwrap()).render();
        assert!(rendered.starts_with(r#"{"streams":[{"index":0,"id":"","codec_name":"pcm_s16le""#));
        // Missing fields keep their Go zero values.
        assert!(rendered.contains(r#""start_pts":0,"start_time":"","duration_ts":16000"#));
        assert!(rendered.contains(r#""attached_pic":0},"tags":null,"width":0,"height":0,"sample_fmt":"s16","sample_rate":"8000","channels":1,"bits_per_sample":16}]"#));
        assert!(rendered.contains(r#""start_time":"0","duration":"2","size":"32044""#));
        assert!(rendered.ends_with(r#""probe_score":99,"tags":null},"chapters":[]}"#));
    }

    #[test]
    fn side_data_uses_the_typed_structs() {
        let value: Value = serde_json::from_str(
            r#"{"side_data_type": "Mastering display metadata", "red_x": "17/25", "max_luminance": "4000/1"}"#,
        )
        .unwrap();
        assert_eq!(
            side_data(&value).render(),
            r#"{"side_data_type":"Mastering display metadata","red_x":0.68,"max_luminance":4000}"#
        );
        let unknown: Value =
            serde_json::from_str(r#"{"side_data_type": "Other", "b": 2, "a": "x"}"#).unwrap();
        assert_eq!(
            side_data(&unknown).render(),
            r#"{"a":"x","b":2,"side_data_type":"Other"}"#
        );
    }
}
