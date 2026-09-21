use std::{fmt, str::FromStr};

use crate::Error;

const BYTES: usize = 20;
const HEX_LEN: usize = BYTES * 2;

/// BitTorrent v1 info hash: the SHA-1 of the bencoded `info` dictionary.
///
/// This is the identity of a torrent throughout Rustorr, as it is in the
/// TorrServer API. Text form is 40 lowercase hex characters; parsing also
/// accepts uppercase.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InfoHash([u8; BYTES]);

impl InfoHash {
    pub const fn from_bytes(bytes: [u8; BYTES]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; BYTES] {
        &self.0
    }
}

impl FromStr for InfoHash {
    type Err = Error;

    fn from_str(text: &str) -> Result<Self, Error> {
        let actual = text.chars().count();
        if actual != HEX_LEN {
            return Err(Error::InfoHashLength {
                expected: HEX_LEN,
                actual,
            });
        }

        let mut digits = [0u8; HEX_LEN];
        for (position, character) in text.chars().enumerate() {
            let digit = character.to_digit(16).ok_or(Error::InfoHashCharacter {
                position,
                character,
            })?;
            digits[position] = digit as u8;
        }

        let mut bytes = [0u8; BYTES];
        for (byte, pair) in bytes.iter_mut().zip(digits.chunks_exact(2)) {
            *byte = (pair[0] << 4) | pair[1];
        }
        Ok(Self(bytes))
    }
}

impl fmt::Display for InfoHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in &self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for InfoHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "InfoHash({self})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Reported by the TorrServer reference for the R1 single-file fixture.
    const REFERENCE: &str = "68c3ccdd52b2925f4f97e2f61ea248e728e54cea";

    #[test]
    fn parses_reference_hash_and_prints_it_back_unchanged() {
        let hash: InfoHash = REFERENCE.parse().unwrap();
        assert_eq!(hash.as_bytes()[0], 0x68);
        assert_eq!(hash.as_bytes()[1], 0xc3);
        assert_eq!(hash.as_bytes()[19], 0xea);
        assert_eq!(hash.to_string(), REFERENCE);
    }

    #[test]
    fn uppercase_is_accepted_and_normalised_to_lowercase() {
        let hash: InfoHash = REFERENCE.to_uppercase().parse().unwrap();
        assert_eq!(hash.to_string(), REFERENCE);
        assert_eq!(hash, REFERENCE.parse().unwrap());
    }

    #[test]
    fn bytes_round_trip() {
        let bytes = [0xab; BYTES];
        assert_eq!(InfoHash::from_bytes(bytes).as_bytes(), &bytes);
        assert_eq!(InfoHash::from_bytes(bytes).to_string(), "ab".repeat(BYTES));
    }

    #[test]
    fn rejects_wrong_length() {
        for (text, actual) in [
            ("", 0),
            (&REFERENCE[..39], 39),
            (&format!("{REFERENCE}0"), 41),
        ] {
            assert_eq!(
                text.parse::<InfoHash>(),
                Err(Error::InfoHashLength {
                    expected: 40,
                    actual
                }),
                "{text:?}"
            );
        }
    }

    #[test]
    fn trailing_newline_is_not_trimmed() {
        assert!(matches!(
            format!("{REFERENCE}\n").parse::<InfoHash>(),
            Err(Error::InfoHashLength { actual: 41, .. })
        ));
    }

    #[test]
    fn reports_position_of_first_non_hex_character() {
        let mut text = REFERENCE.to_owned();
        text.replace_range(39..40, "g");
        assert_eq!(
            text.parse::<InfoHash>(),
            Err(Error::InfoHashCharacter {
                position: 39,
                character: 'g'
            })
        );
    }

    #[test]
    fn hex_prefix_is_rejected() {
        let text = format!("0x{}", &REFERENCE[..38]);
        assert_eq!(
            text.parse::<InfoHash>(),
            Err(Error::InfoHashCharacter {
                position: 1,
                character: 'x'
            })
        );
    }

    #[test]
    fn multibyte_character_is_an_error_not_a_panic() {
        // 40 characters, but the last one is two bytes long.
        let text = format!("{}é", &REFERENCE[..39]);
        assert_eq!(
            text.parse::<InfoHash>(),
            Err(Error::InfoHashCharacter {
                position: 39,
                character: 'é'
            })
        );
    }

    #[test]
    fn debug_shows_hex_rather_than_a_byte_array() {
        let hash: InfoHash = REFERENCE.parse().unwrap();
        assert_eq!(format!("{hash:?}"), format!("InfoHash({REFERENCE})"));
    }
}
