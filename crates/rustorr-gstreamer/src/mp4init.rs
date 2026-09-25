//! `server/gstreamer/mp4init.go`: codec strings, size and range of the
//! variant, read from the init segment mp4mux wrote.

#[derive(Debug, Clone, Default, PartialEq)]
pub struct VariantInfo {
    pub codecs: String,
    pub video_range: String,
    pub width: i64,
    pub height: i64,
    pub frame_rate: f64,
}

#[derive(Clone, Copy)]
struct Box<'a> {
    kind: &'a [u8],
    payload_start: usize,
    end: usize,
}

struct Track {
    video: bool,
    codec: String,
    video_range: String,
    width: i64,
    height: i64,
}

/// `readMP4InitInfo`.
pub fn read(data: &[u8]) -> Option<VariantInfo> {
    if data.len() < 8 {
        return None;
    }
    let moov = find_child(data, 0, data.len(), b"moov")?;
    let mut result = VariantInfo::default();
    let mut cursor = moov.payload_start;
    while let Some((item, next)) = read_box(data, cursor, moov.end) {
        cursor = next;
        if item.kind != b"trak" {
            continue;
        }
        let Some(track) = read_track(data, item) else {
            continue;
        };
        if track.codec.is_empty() {
            continue;
        }
        if track.video && result.codecs.is_empty() {
            result.codecs = track.codec;
            result.video_range = track.video_range;
            result.width = track.width;
            result.height = track.height;
        } else if !track.video {
            if result.codecs.is_empty() {
                result.codecs = track.codec;
            } else if !result.codecs.contains("mp4a.") {
                result.codecs = format!("{},{}", result.codecs, track.codec);
            }
        }
    }
    (!result.codecs.is_empty()).then_some(result)
}

fn read_track(data: &[u8], trak: Box<'_>) -> Option<Track> {
    let mdia = find_child(data, trak.payload_start, trak.end, b"mdia")?;
    let hdlr = find_child(data, mdia.payload_start, mdia.end, b"hdlr")?;
    if hdlr.payload_start + 12 > hdlr.end {
        return None;
    }
    let handler = &data[hdlr.payload_start + 8..hdlr.payload_start + 12];
    let video = handler == b"vide";
    if !video && handler != b"soun" {
        return None;
    }
    let minf = find_child(data, mdia.payload_start, mdia.end, b"minf")?;
    let stbl = find_child(data, minf.payload_start, minf.end, b"stbl")?;
    let stsd = find_child(data, stbl.payload_start, stbl.end, b"stsd")?;
    if stsd.payload_start + 8 > stsd.end {
        return None;
    }
    let count = u32_at(data, stsd.payload_start + 4);
    let mut cursor = stsd.payload_start + 8;
    for _ in 0..count {
        let Some((entry, next)) = read_box(data, cursor, stsd.end) else {
            break;
        };
        cursor = next;
        let track = if video {
            video_entry(data, entry)
        } else {
            audio_entry(data, entry)
        };
        if track.is_some() {
            return track;
        }
    }
    None
}

fn video_entry(data: &[u8], entry: Box<'_>) -> Option<Track> {
    let fixed_end = entry.payload_start + 78;
    if fixed_end > entry.end {
        return None;
    }
    let config_type: &[u8] = match entry.kind {
        b"avc1" | b"avc3" => b"avcC",
        b"hvc1" | b"hev1" => b"hvcC",
        b"av01" => b"av1C",
        b"vp09" => b"vpcC",
        _ => return None,
    };
    let config = find_child(data, fixed_end, entry.end, config_type)?;
    let payload = &data[config.payload_start..config.end];
    let sample_entry = String::from_utf8_lossy(entry.kind);
    let codec = match entry.kind {
        b"avc1" | b"avc3" if payload.len() >= 4 => format!(
            "{sample_entry}.{:02X}{:02X}{:02X}",
            payload[1], payload[2], payload[3]
        ),
        b"hvc1" | b"hev1" => hevc_codec(&sample_entry, payload),
        b"av01" => av1_codec(payload),
        b"vp09" => vp9_codec(payload),
        _ => String::new(),
    };
    if codec.is_empty() {
        return None;
    }
    let video_range = find_child(data, fixed_end, entry.end, b"colr")
        .map(|colr| video_range(&data[colr.payload_start..colr.end]))
        .unwrap_or_default();
    Some(Track {
        video: true,
        codec,
        video_range,
        width: i64::from(u16_at(data, entry.payload_start + 24)),
        height: i64::from(u16_at(data, entry.payload_start + 26)),
    })
}

fn audio_entry(data: &[u8], entry: Box<'_>) -> Option<Track> {
    if entry.kind != b"mp4a" || entry.payload_start + 28 > entry.end {
        return None;
    }
    let child_start = match u16_at(data, entry.payload_start + 8) {
        1 => entry.payload_start + 44,
        2 => entry.payload_start + 64,
        _ => entry.payload_start + 28,
    };
    let esds = find_child(data, child_start, entry.end, b"esds")?;
    let codec = aac_codec(&data[esds.payload_start..esds.end]);
    (!codec.is_empty()).then(|| Track {
        video: false,
        codec,
        video_range: String::new(),
        width: 0,
        height: 0,
    })
}

fn hevc_codec(sample_entry: &str, data: &[u8]) -> String {
    if data.len() < 13 {
        return String::new();
    }
    let profile_byte = data[1];
    let space = ["", "A", "B", "C"][usize::from(profile_byte >> 6)];
    let compatibility = u32::from_be_bytes([data[2], data[3], data[4], data[5]]).reverse_bits();
    let tier = if profile_byte & 0x20 != 0 { "H" } else { "L" };
    let mut result = format!(
        "{sample_entry}.{space}{}.{compatibility}.{tier}{}",
        profile_byte & 0x1f,
        data[12]
    );
    let mut last = 11;
    while last >= 6 && data[last] == 0 {
        last -= 1;
    }
    if last >= 6 {
        for byte in &data[6..=last] {
            result.push_str(&format!(".{byte:02X}"));
        }
    }
    result
}

fn av1_codec(data: &[u8]) -> String {
    if data.len() < 3 || data[0] & 0x80 == 0 {
        return String::new();
    }
    let profile = data[1] >> 5;
    let tier = if data[2] & 0x80 != 0 { "H" } else { "M" };
    let depth = if data[2] & 0x40 == 0 {
        8
    } else if profile == 2 && data[2] & 0x20 != 0 {
        12
    } else {
        10
    };
    format!("av01.{profile}.{:02}{tier}.{depth:02}", data[1] & 0x1f)
}

fn vp9_codec(data: &[u8]) -> String {
    let offset = if data.len() >= 7 && data[0] <= 1 && data[1..4] == [0, 0, 0] {
        4
    } else {
        0
    };
    if data.len() < offset + 3 {
        return String::new();
    }
    let mut depth = data[offset + 2];
    if depth > 16 {
        depth >>= 4;
    }
    format!(
        "vp09.{:02}.{:02}.{depth:02}",
        data[offset],
        data[offset + 1]
    )
}

fn aac_codec(data: &[u8]) -> String {
    let start = data.len().min(4);
    for index in start..data.len().saturating_sub(2) {
        if data[index] != 0x05 {
            continue;
        }
        let mut cursor = index + 1;
        let Some(length) = descriptor_length(data, &mut cursor) else {
            continue;
        };
        if length < 2 || cursor + length > data.len() {
            continue;
        }
        let mut object = usize::from(data[cursor] >> 3);
        if object == 31 {
            object = 32 + (usize::from(data[cursor] & 7) << 3) + usize::from(data[cursor + 1] >> 5);
        }
        return format!("mp4a.40.{object}");
    }
    String::new()
}

fn descriptor_length(data: &[u8], cursor: &mut usize) -> Option<usize> {
    let mut length = 0;
    for _ in 0..4 {
        let value = *data.get(*cursor)?;
        *cursor += 1;
        length = length << 7 | usize::from(value & 0x7f);
        if value & 0x80 == 0 {
            return Some(length);
        }
    }
    None
}

fn video_range(data: &[u8]) -> String {
    if data.len() < 10 || (&data[..4] != b"nclx" && &data[..4] != b"nclc") {
        return String::new();
    }
    match u16_at(data, 6) {
        16 => "PQ",
        18 => "HLG",
        1 | 4 | 5 | 6 | 7 | 8 | 13 | 14 | 15 => "SDR",
        _ => "",
    }
    .into()
}

fn find_child<'a>(data: &'a [u8], start: usize, end: usize, kind: &[u8]) -> Option<Box<'a>> {
    let mut cursor = start;
    loop {
        let (item, next) = read_box(data, cursor, end)?;
        if item.kind == kind {
            return Some(item);
        }
        cursor = next;
    }
}

fn read_box(data: &[u8], cursor: usize, end: usize) -> Option<(Box<'_>, usize)> {
    if end > data.len() || cursor + 8 > end {
        return None;
    }
    let size32 = u32_at(data, cursor);
    let kind = &data[cursor + 4..cursor + 8];
    let (size, header) = match size32 {
        1 => {
            if cursor + 16 > end {
                return None;
            }
            let size = u64::from_be_bytes(data[cursor + 8..cursor + 16].try_into().ok()?);
            (size, 16)
        }
        0 => ((end - cursor) as u64, 8),
        size => (u64::from(size), 8),
    };
    if size < header as u64 || size > (end - cursor) as u64 {
        return None;
    }
    let box_end = cursor + size as usize;
    Some((
        Box {
            kind,
            payload_start: cursor + header,
            end: box_end,
        },
        box_end,
    ))
}

fn u32_at(data: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes(data[offset..offset + 4].try_into().expect("four bytes"))
}

fn u16_at(data: &[u8], offset: usize) -> u16 {
    u16::from_be_bytes(data[offset..offset + 2].try_into().expect("two bytes"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mp4_box(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        [
            &((payload.len() + 8) as u32).to_be_bytes()[..],
            kind,
            payload,
        ]
        .concat()
    }

    fn track(handler: &[u8; 4], entry: Vec<u8>) -> Vec<u8> {
        let hdlr = mp4_box(b"hdlr", &[&[0; 8][..], handler, &[0; 12]].concat());
        let stsd = mp4_box(b"stsd", &[&[0, 0, 0, 0, 0, 0, 0, 1][..], &entry].concat());
        let stbl = mp4_box(b"stbl", &stsd);
        let minf = mp4_box(b"minf", &stbl);
        mp4_box(b"trak", &mp4_box(b"mdia", &[hdlr, minf].concat()))
    }

    fn avc1(width: u16, height: u16) -> Vec<u8> {
        let mut fixed = vec![0u8; 78];
        fixed[24..26].copy_from_slice(&width.to_be_bytes());
        fixed[26..28].copy_from_slice(&height.to_be_bytes());
        let avcc = mp4_box(b"avcC", &[1, 0x42, 0xC0, 0x0A, 0xFF]);
        let colr = mp4_box(
            b"colr",
            &[b"nclx".as_slice(), &[0, 1, 0, 1, 0, 1, 0]].concat(),
        );
        mp4_box(b"avc1", &[fixed, avcc, colr].concat())
    }

    fn mp4a() -> Vec<u8> {
        // ES_Descriptor > DecoderConfigDescriptor > DecoderSpecificInfo (AAC LC).
        let esds = mp4_box(
            b"esds",
            &[
                &[0u8; 4][..],
                &[0x03, 0x19, 0, 1, 0, 0x04, 0x11, 0x40, 0x15],
                &[0; 12],
                &[0x05, 0x02, 0x15, 0x88],
            ]
            .concat(),
        );
        mp4_box(b"mp4a", &[vec![0u8; 28], esds].concat())
    }

    #[test]
    fn codecs_and_size_come_from_the_sample_entries() {
        let init = [
            mp4_box(b"ftyp", b"iso6"),
            mp4_box(
                b"moov",
                &[track(b"vide", avc1(64, 48)), track(b"soun", mp4a())].concat(),
            ),
        ]
        .concat();
        assert_eq!(
            read(&init),
            Some(VariantInfo {
                codecs: "avc1.42C00A,mp4a.40.2".into(),
                video_range: "SDR".into(),
                width: 64,
                height: 48,
                frame_rate: 0.0,
            })
        );
        assert_eq!(read(&mp4_box(b"moov", b"")), None);
    }

    #[test]
    fn codec_strings_match_the_reference() {
        assert_eq!(av1_codec(&[0x81, 0x08, 0x0C]), "av01.0.08M.08");
        assert_eq!(vp9_codec(&[1, 0, 0, 0, 0, 10, 0x80]), "vp09.00.10.08");
        let mut hvcc = [0u8; 23];
        hvcc[1] = 0x01;
        hvcc[2] = 0x60;
        hvcc[6] = 0x90;
        hvcc[12] = 93;
        assert_eq!(hevc_codec("hvc1", &hvcc), "hvc1.1.6.L93.90");
    }
}
