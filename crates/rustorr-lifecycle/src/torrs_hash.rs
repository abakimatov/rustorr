use std::io::{Read, Write};

use flate2::{Compression, read::ZlibDecoder, write::ZlibEncoder};
use num_bigint::BigUint;

use crate::TorrentView;

const ALPHABET: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

fn field(buffer: &mut Vec<u8>, tag: u8, value: &str) {
    let value = value.trim();
    if value.is_empty() {
        return;
    }
    buffer.push(tag);
    buffer.extend_from_slice(&u16::try_from(value.len()).unwrap_or(u16::MAX).to_le_bytes());
    buffer.extend_from_slice(value.as_bytes());
}

fn base62(bytes: &[u8]) -> String {
    let mut value = BigUint::from_bytes_be(bytes);
    let radix = BigUint::from(62u8);
    let mut encoded = Vec::new();
    while value != BigUint::default() {
        let remainder = (&value % &radix).to_u32_digits();
        let digit = remainder.first().copied().unwrap_or_default() as usize;
        encoded.push(ALPHABET[digit]);
        value /= &radix;
    }
    encoded.reverse();
    String::from_utf8(encoded).expect("base62 is ASCII")
}

fn unbase62(token: &str) -> Option<Vec<u8>> {
    let radix = BigUint::from(62u8);
    let mut value = BigUint::default();
    for byte in token.trim().bytes() {
        let digit = ALPHABET.iter().position(|candidate| *candidate == byte)?;
        value = value * &radix + BigUint::from(digit);
    }
    Some(value.to_bytes_be())
}

fn matrix_zlib(plain: &[u8]) -> Option<Vec<u8>> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::best());
    encoder.write_all(plain).ok()?;
    // Go's compress/flate emits the data block before Close and terminates it
    // with a final empty stored block. zlib-rs emits the same data block on a
    // sync flush, then appends a separate empty final block on finish. Convert
    // only that framing so torrs_hash tokens are byte-for-byte MatriX.145.
    encoder.flush().ok()?;
    let mut packed = encoder.finish().ok()?;
    let sync_len = packed.len().checked_sub(6)?;
    if packed.get(sync_len..sync_len + 2)? != [0x03, 0x00] {
        return None;
    }
    packed.drain(sync_len..sync_len + 2);
    let header = sync_len.checked_sub(5)?;
    for bit in 0..8 {
        if packed[header] & (1 << bit) != 0 {
            continue;
        }
        let mut candidate = packed.clone();
        candidate[header] |= 1 << bit;
        let mut decoded = Vec::new();
        if ZlibDecoder::new(candidate.as_slice())
            .read_to_end(&mut decoded)
            .is_ok()
            && decoded == plain
        {
            return Some(candidate);
        }
    }
    None
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decoded {
    pub hash: rustorr_domain::InfoHash,
    pub title: String,
    pub poster: String,
    pub category: String,
    pub trackers: Vec<String>,
}

pub fn decode(token: &str) -> Option<Decoded> {
    let token = token.strip_prefix("torrs://").unwrap_or(token);
    let packed = unbase62(token)?;
    let mut plain = Vec::new();
    ZlibDecoder::new(packed.as_slice())
        .read_to_end(&mut plain)
        .ok()?;
    let hash = rustorr_domain::InfoHash::from_bytes(plain.get(..20)?.try_into().ok()?);
    let mut result = Decoded {
        hash,
        title: String::new(),
        poster: String::new(),
        category: String::new(),
        trackers: Vec::new(),
    };
    let mut position = 20;
    while position < plain.len() {
        let tag = *plain.get(position)?;
        position += 1;
        if tag == 4 {
            position = position.checked_add(8)?;
            if position > plain.len() {
                return None;
            }
            continue;
        }
        let length =
            u16::from_le_bytes(plain.get(position..position + 2)?.try_into().ok()?) as usize;
        position += 2;
        let value = std::str::from_utf8(plain.get(position..position + length)?).ok()?;
        position += length;
        match tag {
            0 => result.title = value.into(),
            1 => result.poster = value.into(),
            2 => result.trackers.push(value.into()),
            3 => result.category = value.into(),
            _ => {}
        }
    }
    Some(result)
}

pub fn encode(view: &TorrentView, trackers: &[String]) -> Option<String> {
    let hash = view.hash()?;
    let mut plain = Vec::from(hash.as_bytes());
    field(&mut plain, 0, &view.title);
    field(&mut plain, 1, &view.poster);
    field(&mut plain, 3, &view.category);
    if let Some(size) = view.torrent_size.filter(|size| *size != 0) {
        plain.push(4);
        plain.extend_from_slice(&i64::try_from(size).ok()?.to_le_bytes());
    }
    for tracker in trackers {
        field(&mut plain, 2, tracker);
    }
    Some(base62(&matrix_zlib(&plain)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SINGLE_TOKEN: &str = "GWwwChbrimwdztSH8LLO4k26cjpjUzG6MVyKFTDKkHdwynzuleMZ0nc2XXaKilT4xIdmAYmpSvOYPCe5FeeI4WoF40FedM3PkciYurTqesqs9Eaob";

    #[test]
    fn matrix_145_single_file_golden_vector_round_trips() {
        let decoded = decode(SINGLE_TOKEN).unwrap();
        assert_eq!(
            decoded.hash.to_string(),
            "68c3ccdd52b2925f4f97e2f61ea248e728e54cea"
        );
        assert_eq!(decoded.title, "single.bin");
        assert_eq!(decoded.trackers, ["http://tracker:6969/announce"]);

        let view = TorrentView {
            title: decoded.title,
            category: decoded.category,
            poster: decoded.poster,
            data: None,
            timestamp: 0,
            name: Some("single.bin".into()),
            hash: Some(decoded.hash.to_string()),
            torrs_hash: None,
            stat: 3,
            stat_string: "Torrent working".into(),
            loaded_size: None,
            torrent_size: Some(8_388_608),
            download_speed: None,
            upload_speed: None,
            total_peers: None,
            active_peers: None,
            connected_seeders: None,
            bytes_written: None,
            bytes_read: None,
            file_stats: Vec::new(),
        };
        assert_eq!(
            encode(&view, &decoded.trackers).as_deref(),
            Some(SINGLE_TOKEN)
        );
    }
}
