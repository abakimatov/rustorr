//! `json.NewDecoder(body).Decode(&v)` as gin's `BindJSON` runs it: only the
//! first JSON value counts, trailing bytes are ignored, and errors carry Go's
//! exact texts, because some MatriX.145 routes echo them to the client.

use serde::de::{Deserialize, Deserializer, MapAccess, Visitor};
use serde_json::Value;

/// Decodes into a Go `string`. `null` leaves the target untouched (`None`).
pub(crate) fn string(input: &[u8]) -> Result<Option<String>, String> {
    match first_value(input)? {
        Value::Null => Ok(None),
        Value::String(text) => Ok(Some(text)),
        other => Err(format!(
            "json: cannot unmarshal {} into Go value of type string",
            kind(&other)
        )),
    }
}

/// Decodes into `struct{ Data string }`: keys match the field name exactly
/// or case-insensitively, and the last matching key wins.
pub(crate) fn data_field(input: &[u8]) -> Result<String, String> {
    const TARGET: &str = "struct { Data string }";
    let value = first_value(input)?;
    let Value::Object(_) = value else {
        return match value {
            Value::Null => Ok(String::new()),
            other => Err(format!(
                "json: cannot unmarshal {} into Go value of type {TARGET}",
                kind(&other)
            )),
        };
    };
    let OrderedObject(entries) = ordered(input)?;
    let mut data = String::new();
    let mut error = None;
    for (key, value) in entries {
        if !key.eq_ignore_ascii_case("data") {
            continue;
        }
        match value {
            Value::String(text) => data = text,
            Value::Null => {}
            other => {
                error.get_or_insert_with(|| {
                    format!(
                        "json: cannot unmarshal {} into Go struct field .Data of type string",
                        kind(&other)
                    )
                });
            }
        }
    }
    error.map_or(Ok(data), Err)
}

fn kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn first_value(input: &[u8]) -> Result<Value, String> {
    let end = scan(input)?;
    serde_json::from_slice(&input[..end]).map_err(|error| error.to_string())
}

fn ordered(input: &[u8]) -> Result<OrderedObject, String> {
    let end = scan(input)?;
    serde_json::from_slice(&input[..end]).map_err(|error| error.to_string())
}

/// An object's members in document order, duplicates kept.
struct OrderedObject(Vec<(String, Value)>);

impl<'de> Deserialize<'de> for OrderedObject {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Members;
        impl<'de> Visitor<'de> for Members {
            type Value = OrderedObject;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a JSON object")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<OrderedObject, A::Error> {
                let mut entries = Vec::new();
                while let Some(entry) = map.next_entry()? {
                    entries.push(entry);
                }
                Ok(OrderedObject(entries))
            }
        }
        deserializer.deserialize_map(Members)
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Parse {
    ObjectKey,
    ObjectValue,
    ArrayValue,
}

#[derive(Clone, Copy)]
enum State {
    BeginValue,
    BeginValueOrEmpty,
    BeginStringOrEmpty,
    BeginString,
    EndValue,
    InString,
    InStringEsc,
    InStringEscU(u8),
    Neg,
    One,
    Zero,
    Dot,
    Dot0,
    E,
    ESign,
    E0,
    Literal(&'static [u8], usize),
}

/// Go's `encoding/json` scanner over the first value: the byte length of
/// that value, or the error `Decoder.Decode` reports.
fn scan(input: &[u8]) -> Result<usize, String> {
    let mut stack: Vec<Parse> = Vec::new();
    let mut state = State::BeginValue;
    let mut started = false;
    for (index, &byte) in input.iter().enumerate() {
        let space = matches!(byte, b' ' | b'\t' | b'\r' | b'\n');
        started |= !space;
        state = match step(state, byte, space, &mut stack)? {
            Step::Continue(next) => next,
            // The closing bracket of the top-level value completes it.
            Step::Closed if stack.is_empty() => return Ok(index + 1),
            Step::Closed => State::EndValue,
            // A literal, number or string ends at the first byte after it.
            Step::TopEnd => return Ok(index),
        };
    }
    match state {
        State::EndValue if stack.is_empty() && started => Ok(input.len()),
        State::One | State::Zero | State::Dot0 | State::E0 if stack.is_empty() => Ok(input.len()),
        _ if started => Err("unexpected EOF".into()),
        _ => Err("EOF".into()),
    }
}

enum Step {
    Continue(State),
    Closed,
    TopEnd,
}

fn step(state: State, byte: u8, space: bool, stack: &mut Vec<Parse>) -> Result<Step, String> {
    use State::*;
    let next = match state {
        BeginValueOrEmpty if space => BeginValueOrEmpty,
        BeginValueOrEmpty if byte == b']' => return close(stack),
        BeginValueOrEmpty | BeginValue => {
            if space {
                return Ok(Step::Continue(BeginValue));
            }
            match byte {
                b'{' => {
                    stack.push(Parse::ObjectKey);
                    BeginStringOrEmpty
                }
                b'[' => {
                    stack.push(Parse::ArrayValue);
                    BeginValueOrEmpty
                }
                b'"' => InString,
                b'-' => Neg,
                b'0' => Zero,
                b'1'..=b'9' => One,
                b't' => Literal(b"true", 1),
                b'f' => Literal(b"false", 1),
                b'n' => Literal(b"null", 1),
                _ => return Err(invalid(byte, "looking for beginning of value")),
            }
        }
        BeginStringOrEmpty if space => BeginStringOrEmpty,
        BeginStringOrEmpty if byte == b'}' => return close(stack),
        BeginStringOrEmpty | BeginString => match byte {
            _ if space => BeginString,
            b'"' => InString,
            _ => return Err(invalid(byte, "looking for beginning of object key string")),
        },
        EndValue => return end_value(byte, space, stack),
        InString => match byte {
            b'"' => EndValue,
            b'\\' => InStringEsc,
            0..0x20 => return Err(invalid(byte, "in string literal")),
            _ => InString,
        },
        InStringEsc => match byte {
            b'b' | b'f' | b'n' | b'r' | b't' | b'\\' | b'/' | b'"' => InString,
            b'u' => InStringEscU(0),
            _ => return Err(invalid(byte, "in string escape code")),
        },
        InStringEscU(seen) if byte.is_ascii_hexdigit() => {
            if seen == 3 {
                InString
            } else {
                InStringEscU(seen + 1)
            }
        }
        InStringEscU(_) => return Err(invalid(byte, "in \\u hexadecimal character escape")),
        Neg => match byte {
            b'0' => Zero,
            b'1'..=b'9' => One,
            _ => return Err(invalid(byte, "in numeric literal")),
        },
        One if byte.is_ascii_digit() => One,
        One | Zero => match byte {
            b'.' => Dot,
            b'e' | b'E' => E,
            _ => return end_value(byte, space, stack),
        },
        Dot if byte.is_ascii_digit() => Dot0,
        Dot => return Err(invalid(byte, "after decimal point in numeric literal")),
        Dot0 => match byte {
            b'0'..=b'9' => Dot0,
            b'e' | b'E' => E,
            _ => return end_value(byte, space, stack),
        },
        E if matches!(byte, b'+' | b'-') => ESign,
        E | ESign if byte.is_ascii_digit() => E0,
        E | ESign => return Err(invalid(byte, "in exponent of numeric literal")),
        E0 if byte.is_ascii_digit() => E0,
        E0 => return end_value(byte, space, stack),
        Literal(word, seen) => {
            if byte != word[seen] {
                return Err(invalid(
                    byte,
                    &format!(
                        "in literal {} (expecting '{}')",
                        std::str::from_utf8(word).expect("ASCII literal"),
                        char::from(word[seen])
                    ),
                ));
            }
            if seen + 1 == word.len() {
                EndValue
            } else {
                Literal(word, seen + 1)
            }
        }
    };
    Ok(Step::Continue(next))
}

fn end_value(byte: u8, space: bool, stack: &mut Vec<Parse>) -> Result<Step, String> {
    let Some(top) = stack.last_mut() else {
        return Ok(Step::TopEnd);
    };
    if space {
        return Ok(Step::Continue(State::EndValue));
    }
    match (*top, byte) {
        (Parse::ObjectKey, b':') => {
            *top = Parse::ObjectValue;
            Ok(Step::Continue(State::BeginValue))
        }
        (Parse::ObjectKey, _) => Err(invalid(byte, "after object key")),
        (Parse::ObjectValue, b',') => {
            *top = Parse::ObjectKey;
            Ok(Step::Continue(State::BeginString))
        }
        (Parse::ObjectValue, b'}') | (Parse::ArrayValue, b']') => close(stack),
        (Parse::ObjectValue, _) => Err(invalid(byte, "after object key:value pair")),
        (Parse::ArrayValue, b',') => Ok(Step::Continue(State::BeginValue)),
        (Parse::ArrayValue, _) => Err(invalid(byte, "after array element")),
    }
}

fn close(stack: &mut Vec<Parse>) -> Result<Step, String> {
    stack.pop();
    Ok(Step::Closed)
}

/// `"invalid character " + quoteChar(c) + " " + context`.
fn invalid(byte: u8, context: &str) -> String {
    let quoted = match byte {
        b'\'' => "'\\''".to_owned(),
        b'"' => "'\"'".to_owned(),
        _ => {
            // strconv.Quote of the byte as a Latin-1 rune, outer quotes
            // swapped.
            let text = char::from(byte).to_string();
            let escaped = match byte {
                0x07 => "\\a".to_owned(),
                0x08 => "\\b".to_owned(),
                0x0c => "\\f".to_owned(),
                b'\n' => "\\n".to_owned(),
                b'\r' => "\\r".to_owned(),
                b'\t' => "\\t".to_owned(),
                0x0b => "\\v".to_owned(),
                b'\\' => "\\\\".to_owned(),
                0..0x20 | 0x7f => format!("\\x{byte:02x}"),
                0x80..=0xa0 | 0xad => format!("\\u{byte:04x}"),
                _ => text,
            };
            format!("'{escaped}'")
        }
    };
    format!("invalid character {quoted} {context}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_and_truncated_input_report_go_eof_errors() {
        assert_eq!(string(b"").unwrap_err(), "EOF");
        assert_eq!(string(b"  \n").unwrap_err(), "EOF");
        assert_eq!(string(b"\"abc").unwrap_err(), "unexpected EOF");
        assert_eq!(data_field(b"{").unwrap_err(), "unexpected EOF");
        assert_eq!(data_field(b"{\"data\":").unwrap_err(), "unexpected EOF");
    }

    #[test]
    fn syntax_errors_use_the_go_scanner_texts() {
        // Observed from MatriX.145 through POST /msx/trn.
        assert_eq!(
            data_field(b"nope").unwrap_err(),
            "invalid character 'o' in literal null (expecting 'u')"
        );
        assert_eq!(
            string(b"x").unwrap_err(),
            "invalid character 'x' looking for beginning of value"
        );
        assert_eq!(
            data_field(b"{x}").unwrap_err(),
            "invalid character 'x' looking for beginning of object key string"
        );
        assert_eq!(
            data_field(b"{\"a\" 1}").unwrap_err(),
            "invalid character '1' after object key"
        );
        assert_eq!(
            data_field(b"{\"a\":1 2}").unwrap_err(),
            "invalid character '2' after object key:value pair"
        );
        assert_eq!(
            string(b"[1 2]").unwrap_err(),
            "invalid character '2' after array element"
        );
        assert_eq!(
            string(b"-x").unwrap_err(),
            "invalid character 'x' in numeric literal"
        );
        assert_eq!(
            string(b"\"\\q\"").unwrap_err(),
            "invalid character 'q' in string escape code"
        );
        assert_eq!(
            string(b"'a'").unwrap_err(),
            "invalid character '\\'' looking for beginning of value"
        );
    }

    #[test]
    fn only_the_first_value_is_decoded() {
        assert_eq!(string(b"\"a\" trailing").unwrap(), Some("a".into()));
        assert_eq!(data_field(b"{\"data\":\"x\"}{").unwrap(), "x");
        assert_eq!(string(b"null").unwrap(), None);
    }

    #[test]
    fn type_mismatches_name_the_go_target() {
        // Observed from MatriX.145 through POST /msx/trn.
        assert_eq!(
            data_field(br#"{"data":7}"#).unwrap_err(),
            "json: cannot unmarshal number into Go struct field .Data of type string"
        );
        assert_eq!(
            data_field(b"[]").unwrap_err(),
            "json: cannot unmarshal array into Go value of type struct { Data string }"
        );
        assert_eq!(
            string(b"{}").unwrap_err(),
            "json: cannot unmarshal object into Go value of type string"
        );
        assert_eq!(
            string(b"12").unwrap_err(),
            "json: cannot unmarshal number into Go value of type string"
        );
    }

    #[test]
    fn the_data_field_matches_case_insensitively_and_the_last_key_wins() {
        assert_eq!(data_field(br#"{"Data":"a"}"#).unwrap(), "a");
        assert_eq!(data_field(br#"{"DATA":"a","data":"b"}"#).unwrap(), "b");
        assert_eq!(data_field(br#"{"other":1}"#).unwrap(), "");
        assert_eq!(data_field(br#"{"data":null}"#).unwrap(), "");
        assert_eq!(data_field(b"null").unwrap(), "");
    }
}
