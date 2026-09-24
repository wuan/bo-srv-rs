//! Blitzortung websocket message decompression (port of
//! `blitzortung/websocket.py`).
//!
//! The server sends JSON where each character is a code point: codes below
//! 256 are literal characters, codes at or above 256 are LZW-style table
//! references that expand to previously seen substrings.

use std::collections::HashMap;

/// Decompress a websocket message (`websocket.decode`).
///
/// The algorithm mirrors the Python implementation byte-for-code-point: the
/// table starts at `o = 256`, `c` tracks the first character of the previous
/// expansion, and `f` the previous expansion itself.
#[allow(clippy::explicit_counter_loop)]
pub fn decode(data: &str) -> String {
    if data.is_empty() {
        return String::new();
    }

    let h: u32 = 256;
    let mut e: HashMap<u32, String> = HashMap::new();

    let mut chars = data.chars();
    let first = chars.next().expect("non-empty");
    let mut c: char = first;
    let mut f: String = first.to_string();
    let mut g: String = f.clone();
    // `o` is the dictionary insertion pointer (Python's `o`); it advances by
    // one per expanded code.
    let mut o: u32 = h;

    for character in chars {
        let a: u32 = character as u32;
        let value: String = if h > a {
            character.to_string()
        } else {
            e.get(&a).cloned().unwrap_or_else(|| {
                let mut fallback = f.clone();
                fallback.push(c);
                fallback
            })
        };
        g.push_str(&value);

        c = value.chars().next().unwrap_or('\0');
        let mut entry = f.clone();
        entry.push(c);
        e.insert(o, entry);
        o += 1;
        f = value;
    }

    g
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_empty_string() {
        assert_eq!(decode(""), "");
    }

    #[test]
    fn decode_single_char() {
        assert_eq!(decode("a"), "a");
    }

    #[test]
    fn decode_two_chars() {
        assert_eq!(decode("ab"), "ab");
    }

    #[test]
    fn decode_plain_ascii() {
        assert_eq!(decode("{\"key\":\"value\"}"), "{\"key\":\"value\"}");
    }

    #[test]
    fn decode_with_special_char() {
        // chr(0x0106) == code 262 -> expands via the table.
        let source = format!("{{\"time\":16501358936120880{}}}", '\u{0106}');
        assert_eq!(decode(&source), "{\"time\":16501358936120880\":}");
    }

    /// Reference from `tests/test_websocket.py::test_decode`: the compressed
/// message and its full decompressed JSON.
    #[test]
    fn decode_reference_message_from_python_tests() {
        let source = include_str!("../tests/fixtures/websocket_compressed.txt");
        let expected = include_str!("../tests/fixtures/websocket_decoded.txt");
        let source = source.strip_suffix('\n').unwrap_or(source);
        let expected = expected.strip_suffix('\n').unwrap_or(expected);
        assert_eq!(decode(source), expected);
    }
}