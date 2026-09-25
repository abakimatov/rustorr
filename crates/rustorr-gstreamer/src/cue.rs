//! `server/gstreamer/cue.go`: segment boundaries from a Matroska file's
//! Cues, so that remuxed segments start on the source's own key frames.

use std::{future::Future, time::Duration};

const EBML: u64 = 0x1A45_DFA3;
const SEGMENT: u64 = 0x1853_8067;
const SEEK_HEAD: u64 = 0x114D_9B74;
const SEEK: u64 = 0x4DBB;
const SEEK_ENTRY: u64 = 0x53AB;
const SEEK_POSITION: u64 = 0x53AC;
const INFO: u64 = 0x1549_A966;
const TIMESTAMP_SCALE: u64 = 0x2A_D7B1;
const TRACKS: u64 = 0x1654_AE6B;
const TRACK_ENTRY: u64 = 0xAE;
const TRACK_NUMBER: u64 = 0xD7;
const TRACK_TYPE: u64 = 0x83;
const CUES: u64 = 0x1C53_BB6B;
const CUE_POINT: u64 = 0xBB;
const CUE_TIME: u64 = 0xB3;
const CUE_TRACK_POSITIONS: u64 = 0xB7;
const CUE_TRACK: u64 = 0xF7;
const CUE_CLUSTER_POSITION: u64 = 0xF1;

const DEFAULT_TIMESTAMP_SCALE_NS: u64 = 1_000_000;
const PREFIX_LENGTH: u64 = 4 * 1024 * 1024;
const MAX_METADATA: u64 = 8 * 1024 * 1024;
const MAX_CUES: u64 = 64 * 1024 * 1024;
const MAX_CUE_POINTS: usize = 1_000_000;

/// How long the whole timeline read may take.
pub const READ_TIMEOUT: Duration = Duration::from_secs(45);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CueSegment {
    pub start_ns: u64,
    pub end_ns: u64,
}

impl CueSegment {
    pub fn duration_ns(&self) -> u64 {
        self.end_ns - self.start_ns
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CueTimeline {
    pub segments: Vec<CueSegment>,
    pub timestamp_scale_ns: u64,
    pub max_duration_ns: u64,
}

impl CueTimeline {
    pub fn segment(&self, index: i64) -> Option<CueSegment> {
        usize::try_from(index)
            .ok()
            .and_then(|index| self.segments.get(index).copied())
    }
}

/// `readMatroskaCueTimeline`. `read_range(offset, length)` fetches bytes of
/// the source; any failure means "no timeline", as in the reference.
pub async fn read_timeline<F, Fut>(
    content_length: i64,
    duration_ns: i64,
    read_range: F,
) -> Option<CueTimeline>
where
    F: Fn(u64, u64) -> Fut,
    Fut: Future<Output = Option<Vec<u8>>>,
{
    if duration_ns <= 0 {
        return None;
    }
    let mut prefix_length = PREFIX_LENGTH;
    if content_length > 0 && (content_length as u64) < prefix_length {
        prefix_length = content_length as u64;
    }
    let prefix = read_range(0, prefix_length).await?;
    let segment = find_segment(&prefix)?;
    let segment_data = segment.data_offset as u64;
    let mut positions = Positions::default();
    let mut scale = DEFAULT_TIMESTAMP_SCALE_NS;
    let mut video_track = 0;
    parse_prefix(
        &prefix,
        segment.data_offset,
        &mut positions,
        &mut scale,
        &mut video_track,
    );

    let element = |id: u64, max: u64| {
        let position = positions.get(id);
        let read_range = &read_range;
        async move { read_element(segment_data, position?, id, max, content_length, read_range).await }
    };
    if video_track == 0
        && let Some(data) = element(TRACKS, MAX_METADATA).await
        && let Some(tracks) = read_element_at(&data, 0)
    {
        video_track = video_track_number(&data, tracks);
    }
    if let Some(data) = element(INFO, MAX_METADATA).await
        && let Some(info) = read_element_at(&data, 0)
    {
        parse_timestamp_scale(&data, info, &mut scale);
    }
    positions.get(CUES)?;
    if video_track == 0 || scale == 0 {
        return None;
    }
    let cues = element(CUES, MAX_CUES).await?;
    let root = read_element_at(&cues, 0)?;
    parse_timeline(&cues, root, video_track, scale, duration_ns as u64)
}

async fn read_element<F, Fut>(
    segment_offset: u64,
    relative: u64,
    expected: u64,
    max_length: u64,
    content_length: i64,
    read_range: &F,
) -> Option<Vec<u8>>
where
    F: Fn(u64, u64) -> Fut,
    Fut: Future<Output = Option<Vec<u8>>>,
{
    let offset = segment_offset.checked_add(relative)?;
    if offset > i64::MAX as u64 {
        return None;
    }
    let mut header_length = 16i64;
    if content_length > 0 && content_length - (offset as i64) < header_length {
        header_length = content_length - offset as i64;
    }
    if header_length <= 0 {
        return None;
    }
    let header = read_range(offset, header_length as u64).await?;
    let element = read_element_at(&header, 0)?;
    if element.id != expected || element.unknown_size || element.size > max_length {
        return None;
    }
    let total = (element.data_offset - element.offset) as u64 + element.size;
    if total > max_length + 16 {
        return None;
    }
    if content_length > 0
        && (offset as i64 > content_length || total as i64 > content_length - offset as i64)
    {
        return None;
    }
    read_range(offset, total).await
}

#[derive(Default)]
struct Positions(Vec<(u64, u64)>);

impl Positions {
    fn get(&self, id: u64) -> Option<u64> {
        self.0
            .iter()
            .find(|(target, _)| *target == id)
            .map(|(_, position)| *position)
    }

    /// The first entry for a target wins.
    fn insert(&mut self, id: u64, position: u64) {
        if self.get(id).is_none() {
            self.0.push((id, position));
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Element {
    id: u64,
    offset: usize,
    data_offset: usize,
    size: u64,
    unknown_size: bool,
}

impl Element {
    fn end(&self) -> u64 {
        self.data_offset as u64 + self.size
    }
}

fn parse_prefix(
    data: &[u8],
    mut offset: usize,
    positions: &mut Positions,
    scale: &mut u64,
    video_track: &mut u64,
) {
    while offset < data.len() {
        let Some(element) = read_element_at(data, offset) else {
            return;
        };
        if element.unknown_size || element.end() > data.len() as u64 {
            return;
        }
        match element.id {
            SEEK_HEAD => parse_seek_head(data, element, positions),
            INFO => parse_timestamp_scale(data, element, scale),
            TRACKS => *video_track = video_track_number(data, element),
            _ => {}
        }
        offset = element.end() as usize;
    }
}

/// Children of `parent`, stopping at the first malformed one.
fn children(data: &[u8], parent: Element) -> impl Iterator<Item = Element> + '_ {
    let end = parent.end();
    let mut offset = parent.data_offset;
    std::iter::from_fn(move || {
        if offset as u64 >= end {
            return None;
        }
        let child = read_element_at(data, offset)?;
        if child.unknown_size || child.end() > end {
            return None;
        }
        offset = child.end() as usize;
        Some(child)
    })
}

fn parse_seek_head(data: &[u8], parent: Element, positions: &mut Positions) {
    for entry in children(data, parent) {
        if entry.id != SEEK {
            continue;
        }
        let (mut target, mut position) = (0, None);
        for child in children(data, entry) {
            if child.id == SEEK_ENTRY && child.size > 0 && child.size <= 4 {
                target = unsigned(data, child).unwrap_or(0);
            } else if child.id == SEEK_POSITION && child.size > 0 && child.size <= 8 {
                position = Some(unsigned(data, child).unwrap_or(0));
            }
        }
        if let Some(position) = position
            && target != 0
        {
            positions.insert(target, position);
        }
    }
}

fn parse_timestamp_scale(data: &[u8], parent: Element, result: &mut u64) {
    for child in children(data, parent) {
        if child.id == TIMESTAMP_SCALE && child.size > 0 && child.size <= 8 {
            if let Some(value) = unsigned(data, child)
                && value > 0
            {
                *result = value;
            }
            return;
        }
    }
}

fn video_track_number(data: &[u8], tracks: Element) -> u64 {
    for entry in children(data, tracks) {
        if entry.id != TRACK_ENTRY {
            continue;
        }
        let (mut number, mut kind) = (0, 0);
        for child in children(data, entry) {
            match child.id {
                TRACK_NUMBER => number = unsigned(data, child).unwrap_or(0),
                TRACK_TYPE => kind = unsigned(data, child).unwrap_or(0),
                _ => {}
            }
        }
        if kind == 1 && number > 0 {
            return number;
        }
    }
    0
}

/// `parseCueTimeline`: consecutive video cue times become segments, the last
/// one ending at the file's duration.
fn parse_timeline(
    data: &[u8],
    cues: Element,
    video_track: u64,
    scale: u64,
    duration_ns: u64,
) -> Option<CueTimeline> {
    let mut times = Vec::new();
    let end = cues.end();
    let mut offset = cues.data_offset;
    while (offset as u64) < end {
        let point = read_element_at(data, offset)?;
        if point.unknown_size || point.end() > end {
            return None;
        }
        if point.id == CUE_POINT
            && let Some(time) = video_cue_time(data, point, video_track)
            && time <= u64::MAX / scale
        {
            let time_ns = time * scale;
            if time_ns < duration_ns {
                times.push(time_ns);
                if times.len() > MAX_CUE_POINTS {
                    return None;
                }
            }
        }
        offset = point.end() as usize;
    }
    if times.len() < 2 {
        return None;
    }
    times.sort_unstable();
    let mut segments = Vec::with_capacity(times.len());
    let mut previous = times[0];
    for &current in &times[1..] {
        if current <= previous {
            continue;
        }
        segments.push(CueSegment {
            start_ns: previous,
            end_ns: current,
        });
        previous = current;
    }
    if duration_ns > previous {
        segments.push(CueSegment {
            start_ns: previous,
            end_ns: duration_ns,
        });
    }
    if segments.is_empty() {
        return None;
    }
    let max_duration_ns = segments
        .iter()
        .map(CueSegment::duration_ns)
        .max()
        .unwrap_or(0);
    Some(CueTimeline {
        segments,
        timestamp_scale_ns: scale,
        max_duration_ns,
    })
}

fn video_cue_time(data: &[u8], point: Element, video_track: u64) -> Option<u64> {
    let (mut time, mut has_position) = (None, false);
    let end = point.end();
    let mut offset = point.data_offset;
    while (offset as u64) < end {
        let child = read_element_at(data, offset)?;
        if child.unknown_size || child.end() > end {
            return None;
        }
        if child.id == CUE_TIME {
            time = unsigned(data, child);
        } else if child.id == CUE_TRACK_POSITIONS && has_video_position(data, child, video_track) {
            has_position = true;
        }
        offset = child.end() as usize;
    }
    time.filter(|_| has_position)
}

fn has_video_position(data: &[u8], positions: Element, video_track: u64) -> bool {
    let (mut track, mut has_cluster) = (0, false);
    let end = positions.end();
    let mut offset = positions.data_offset;
    while (offset as u64) < end {
        let Some(child) = read_element_at(data, offset) else {
            return false;
        };
        if child.unknown_size || child.end() > end {
            return false;
        }
        if child.id == CUE_TRACK {
            track = unsigned(data, child).unwrap_or(0);
        } else if child.id == CUE_CLUSTER_POSITION {
            has_cluster = unsigned(data, child).is_some();
        }
        offset = child.end() as usize;
    }
    track == video_track && has_cluster
}

fn find_segment(data: &[u8]) -> Option<Element> {
    let ebml = read_element_at(data, 0)?;
    if ebml.id != EBML || ebml.unknown_size || ebml.end() > data.len() as u64 {
        return None;
    }
    let segment = read_element_at(data, ebml.end() as usize)?;
    (segment.id == SEGMENT).then_some(segment)
}

fn read_element_at(data: &[u8], offset: usize) -> Option<Element> {
    let first = *data.get(offset)?;
    let id_length = vint_length(first, 4)?;
    if data.len() - offset < id_length + 1 {
        return None;
    }
    let id = data[offset..offset + id_length]
        .iter()
        .fold(0u64, |id, byte| id << 8 | u64::from(*byte));
    let size_offset = offset + id_length;
    let size_length = vint_length(data[size_offset], 8)?;
    if data.len() - size_offset < size_length {
        return None;
    }
    let marker = 0x80u8 >> (size_length - 1);
    let size = data[size_offset + 1..size_offset + size_length]
        .iter()
        .fold(
            u64::from(data[size_offset] & marker.wrapping_sub(1)),
            |size, byte| size << 8 | u64::from(*byte),
        );
    let value_bits = size_length * 7;
    let unknown = if value_bits == 56 {
        0x00ff_ffff_ffff_ffff
    } else {
        (1u64 << value_bits) - 1
    };
    Some(Element {
        id,
        offset,
        data_offset: size_offset + size_length,
        size,
        unknown_size: size == unknown,
    })
}

fn vint_length(first: u8, max_length: usize) -> Option<usize> {
    let (mut length, mut mask) = (1, 0x80u8);
    while length <= max_length && first & mask == 0 {
        mask >>= 1;
        length += 1;
    }
    (length <= max_length).then_some(length)
}

fn unsigned(data: &[u8], element: Element) -> Option<u64> {
    if element.size == 0 || element.size > 8 || element.end() > data.len() as u64 {
        return None;
    }
    Some(
        data[element.data_offset..element.end() as usize]
            .iter()
            .fold(0, |value, byte| value << 8 | u64::from(*byte)),
    )
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn vint(size: usize) -> Vec<u8> {
        let mut length = 1;
        while size >= (1 << (7 * length)) - 1 {
            length += 1;
        }
        let value = (size as u64) | (1 << (7 * length));
        value.to_be_bytes()[8 - length..].to_vec()
    }

    pub(crate) fn element(id: u64, payload: &[u8]) -> Vec<u8> {
        let id_bytes = id.to_be_bytes();
        let skip = id_bytes.iter().take_while(|byte| **byte == 0).count();
        [&id_bytes[skip..], &vint(payload.len()), payload].concat()
    }

    fn uint(id: u64, value: u64, width: usize) -> Vec<u8> {
        element(id, &value.to_be_bytes()[8 - width..])
    }

    /// A Matroska skeleton with a video track and cue points at `times` ms.
    fn movie(times: &[u64]) -> Vec<u8> {
        let tracks = element(
            TRACKS,
            &element(
                TRACK_ENTRY,
                &[uint(TRACK_NUMBER, 1, 1), uint(TRACK_TYPE, 1, 1)].concat(),
            ),
        );
        let info = element(INFO, &uint(TIMESTAMP_SCALE, 1_000_000, 3));
        let cues = element(
            CUES,
            &times
                .iter()
                .map(|time| {
                    element(
                        CUE_POINT,
                        &[
                            uint(CUE_TIME, *time, 2),
                            element(
                                CUE_TRACK_POSITIONS,
                                &[uint(CUE_TRACK, 1, 1), uint(CUE_CLUSTER_POSITION, 7, 1)].concat(),
                            ),
                        ]
                        .concat(),
                    )
                })
                .collect::<Vec<_>>()
                .concat(),
        );
        let seek = |id: u64, position: usize| {
            element(
                SEEK,
                &[
                    uint(SEEK_ENTRY, id, 4),
                    uint(SEEK_POSITION, position as u64, 4),
                ]
                .concat(),
            )
        };
        let head_size = element(
            SEEK_HEAD,
            &[seek(INFO, 0), seek(TRACKS, 0), seek(CUES, 0)].concat(),
        )
        .len();
        let head = element(
            SEEK_HEAD,
            &[
                seek(INFO, head_size),
                seek(TRACKS, head_size + info.len()),
                seek(CUES, head_size + info.len() + tracks.len()),
            ]
            .concat(),
        );
        [
            element(EBML, &element(0x4282, b"matroska")),
            element(SEGMENT, &[head, info, tracks, cues].concat()),
        ]
        .concat()
    }

    async fn timeline(data: Vec<u8>, duration_ns: i64) -> Option<CueTimeline> {
        let length = data.len() as i64;
        read_timeline(length, duration_ns, |offset, count| {
            let data = data.clone();
            async move {
                data.get(offset as usize..(offset + count) as usize)
                    .map(<[u8]>::to_vec)
            }
        })
        .await
    }

    #[tokio::test]
    async fn video_cues_become_segments_up_to_the_duration() {
        let result = timeline(movie(&[0, 2000, 4000, 6000]), 8_000_000_000)
            .await
            .unwrap();
        assert_eq!(
            result.segments,
            [0, 2, 4, 6]
                .map(|start| CueSegment {
                    start_ns: start * 1_000_000_000,
                    end_ns: (start + 2) * 1_000_000_000
                })
                .to_vec()
        );
        assert_eq!(result.timestamp_scale_ns, 1_000_000);
        assert_eq!(result.max_duration_ns, 2_000_000_000);
    }

    #[tokio::test]
    async fn one_cue_or_no_duration_is_no_timeline() {
        assert_eq!(timeline(movie(&[0]), 8_000_000_000).await, None);
        assert_eq!(timeline(movie(&[0, 2000]), 0).await, None);
        assert_eq!(
            timeline(b"not matroska".to_vec(), 8_000_000_000).await,
            None
        );
    }

    #[tokio::test]
    async fn cues_at_or_after_the_duration_are_dropped() {
        let result = timeline(movie(&[0, 3000, 9000]), 5_000_000_000)
            .await
            .unwrap();
        assert_eq!(result.segments.len(), 2);
        assert_eq!(result.segments[1].end_ns, 5_000_000_000);
        assert_eq!(result.max_duration_ns, 3_000_000_000);
    }
}
