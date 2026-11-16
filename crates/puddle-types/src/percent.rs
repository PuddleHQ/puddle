// SPDX-License-Identifier: AGPL-3.0-or-later
//! Percent-decoding, once for the crates that read a request. Two readings exist on purpose:
//! [`percent_decoded`] is lenient (it also reads `%uXXXX` and decodes in several rounds) for
//! what a rule must catch however a server reads it, and [`percent_decoded_bytes`] is the strict
//! one-round `%XX` reading for what a swap must find where the server will read it. Both agree
//! on what one `%XX` escape is: [`escape_at`], two ASCII hexadecimal digits and nothing else
//! (not the sign `u8::from_str_radix` would let through).

/// Most percent-decoding rounds: `%252e` is `%2e` after one and `.` after two. A path that is
/// still changing after this many is matched in its last form.
pub(crate) const DECODE_ROUNDS: usize = 4;

/// The value of one ASCII hexadecimal digit, in either case.
#[must_use]
pub fn hex_digit(byte: u8) -> Option<u8> {
    char::from(byte)
        .to_digit(16)
        .and_then(|digit| u8::try_from(digit).ok())
}

/// The byte a `%XX` escape at `at` stands for, or `None` when `raw` has no valid escape there.
///
/// ```
/// assert_eq!(puddle_types::escape_at(b"a%41", 1), Some(b'A'));
/// assert_eq!(puddle_types::escape_at(b"%+1", 0), None);
/// ```
#[must_use]
pub fn escape_at(raw: &[u8], at: usize) -> Option<u8> {
    if raw.get(at) != Some(&b'%') {
        return None;
    }
    let high = hex_digit(*raw.get(at + 1)?)?;
    let low = hex_digit(*raw.get(at + 2)?)?;
    Some(high << 4 | low)
}

/// `raw` with each valid `%XX` escape decoded, in one round; an invalid one stays as written.
///
/// ```
/// assert_eq!(puddle_types::percent_decoded_bytes(b"a%41%2g%"), b"aA%2g%");
/// ```
#[must_use]
pub fn percent_decoded_bytes(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len());
    let mut at = 0;
    while let Some(&byte) = raw.get(at) {
        if let Some(decoded) = escape_at(raw, at) {
            out.push(decoded);
            at += 3;
        } else {
            out.push(byte);
            at += 1;
        }
    }
    out
}

/// One round of percent-decoding, including the `%uXXXX` form some servers accept. An escape
/// that is not valid stays as written.
pub(crate) fn decode_once(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while let Some(&byte) = bytes.get(at) {
        if byte == b'%' {
            if let (Some(b'u' | b'U'), Some(digits)) =
                (bytes.get(at + 1), bytes.get(at + 2..at + 6))
                && let Some(code) = digits.iter().try_fold(0u32, |acc, &d| {
                    hex_digit(d).map(|v| acc * 16 + u32::from(v))
                })
                && let Some(c) = char::from_u32(code)
            {
                let mut buffer = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut buffer).as_bytes());
                at += 6;
                continue;
            }
            if let Some(decoded) = escape_at(bytes, at) {
                out.push(decoded);
                at += 3;
                continue;
            }
        }
        out.push(byte);
        at += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `text` with its percent-escapes decoded, as often as a server might decode them (a few
/// rounds), and nothing else changed. For values a rule reads beside the path, such as a
/// method named in a query string.
///
/// ```
/// assert_eq!(puddle_types::percent_decoded("%44ELETE"), "DELETE");
/// assert_eq!(puddle_types::percent_decoded("%2544ELETE"), "DELETE");
/// ```
#[must_use]
pub fn percent_decoded(text: &str) -> String {
    let mut text = text.to_owned();
    for _ in 0..DECODE_ROUNDS {
        let next = decode_once(&text);
        if next == text {
            break;
        }
        text = next;
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_signed_pair_is_not_an_escape() {
        assert_eq!(escape_at(b"%+1", 0), None);
        assert_eq!(percent_decoded_bytes(b"%+1"), b"%+1");
        assert_eq!(percent_decoded("%+1"), "%+1");
    }

    #[test]
    fn the_strict_and_the_lenient_reading_agree_on_every_plain_escape() {
        for byte in 0..=u8::MAX {
            for text in [format!("%{byte:02x}"), format!("%{byte:02X}")] {
                let strict = percent_decoded_bytes(text.as_bytes());
                assert_eq!(strict, [byte], "{text}");
                let lenient = percent_decoded(&text);
                assert_eq!(
                    lenient.as_bytes(),
                    String::from_utf8_lossy(&strict).as_bytes()
                );
            }
        }
    }

    #[test]
    fn the_strict_reading_leaves_what_only_the_lenient_one_decodes() {
        assert_eq!(percent_decoded_bytes(b"%u0041"), b"%u0041");
        assert_eq!(percent_decoded("%u0041"), "A");
        assert_eq!(percent_decoded_bytes(b"%2541"), b"%41");
        assert_eq!(percent_decoded("%2541"), "A");
    }
}
