// SPDX-License-Identifier: GPL-3.0-or-later
//! Stale stand-ins: values that are shaped like one of Puddle's stand-ins but that this
//! workspace's registry does not hold.
//!
//! A tool keeps using the stand-in it was given after the user removed the secret or forgot the
//! login. The proxy cannot swap it, the server answers `401`, and without a note the user sees an
//! `injected: false` request and nothing else. Two shapes are recognised by their bytes alone:
//!
//! - `puddle-secret-<NAME>-<32 hex>`, the shape of every secret stand-in. The flag names the
//!   secret, a name the guest itself sent.
//! - `gho_`, `ghu_` or `ghr_` followed by 36 base62 characters whose last six are not the CRC-32
//!   checksum of the first thirty. Real GitHub tokens carry that checksum, and Puddle's
//!   stand-ins never do. The flag names nothing but the kind.
//!
//! The flag is an id in the connection record's list, never the value: nothing of a real
//! credential is read, so nothing of it can be shown. A guess is possible, and a stand-in made
//! for another workspace is flagged the same way; the wording says "may".

use memchr::memmem;

/// Prefix of an id for a secret stand-in that is not in the registry.
pub(crate) const STALE_SECRET_PREFIX: &str = "stale-stand-in:secret:";

/// Id for a GitHub-shaped token whose checksum is wrong.
pub(crate) const STALE_GITHUB_ID: &str = "stale-stand-in:github";

const SECRET_MARK: &[u8] = b"puddle-secret-";
const HEX_LEN: usize = 32;
const MAX_NAME: usize = 64;
const BASE62: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
const GITHUB_PREFIXES: [&[u8]; 3] = [b"gho_", b"ghu_", b"ghr_"];
const GITHUB_ENTROPY: usize = 30;
const GITHUB_CHECKSUM: usize = 6;

fn is_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.')
}

/// The ids of the stale stand-ins in `input`, each once. `registered` says whether a piece of
/// `input` is a stand-in the registry holds (such a value is not stale, even inside a longer run
/// of name characters).
pub(crate) fn find(input: &[u8], registered: &dyn Fn(&[u8]) -> bool) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    let mut note = |id: String| {
        if !ids.contains(&id) {
            ids.push(id);
        }
    };
    for at in memmem::find_iter(input, SECRET_MARK) {
        if at > 0 && input.get(at - 1).is_some_and(|b| is_name_byte(*b)) {
            continue;
        }
        let rest = input.get(at + SECRET_MARK.len()..).unwrap_or_default();
        let run = rest.iter().take_while(|b| is_name_byte(**b)).count();
        let token = input
            .get(at..at + SECRET_MARK.len() + run)
            .unwrap_or_default();
        if registered(token) {
            continue;
        }
        if let Some(name) = secret_name(rest.get(..run).unwrap_or_default()) {
            note(format!("{STALE_SECRET_PREFIX}{name}"));
        }
    }
    for prefix in GITHUB_PREFIXES {
        for at in memmem::find_iter(input, prefix) {
            if at > 0 && input.get(at - 1).is_some_and(|b| is_name_byte(*b)) {
                continue;
            }
            let body_at = at + prefix.len();
            let end = body_at + GITHUB_ENTROPY + GITHUB_CHECKSUM;
            let Some(body) = input.get(body_at..end) else {
                continue;
            };
            let bounded = input.get(end).is_none_or(|b| !b.is_ascii_alphanumeric());
            if bounded
                && body.iter().all(u8::is_ascii_alphanumeric)
                && !has_valid_checksum(body)
                && !registered(input.get(at..end).unwrap_or_default())
            {
                note(STALE_GITHUB_ID.to_owned());
            }
        }
    }
    ids
}

/// `NAME` of a run `NAME-<32 hex>`, when it is one.
fn secret_name(run: &[u8]) -> Option<&str> {
    let split = run.len().checked_sub(HEX_LEN + 1)?;
    let (name, tail) = run.split_at(split);
    let hex = tail.strip_prefix(b"-")?;
    let hex_ok = hex.iter().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    if !hex_ok || name.is_empty() || name.len() > MAX_NAME {
        return None;
    }
    std::str::from_utf8(name).ok()
}

/// Whether the last six characters of `body` (36 base62 characters) are the checksum of the
/// first thirty: the CRC-32 of those, in base62 padded with zeros.
fn has_valid_checksum(body: &[u8]) -> bool {
    body.get(..GITHUB_ENTROPY)
        .zip(body.get(GITHUB_ENTROPY..))
        .is_some_and(|(entropy, checksum)| checksum_of(entropy) == checksum)
}

/// The six base62 characters that follow `entropy` in a real token.
fn checksum_of(entropy: &[u8]) -> [u8; GITHUB_CHECKSUM] {
    let mut value = crc32(entropy);
    let mut text = [b'0'; GITHUB_CHECKSUM];
    for slot in text.iter_mut().rev() {
        let digit = usize::try_from(value % 62).unwrap_or(0);
        *slot = BASE62.get(digit).copied().unwrap_or(b'0');
        value /= 62;
    }
    text
}

/// CRC-32 (IEEE, as zlib), bitwise: 30 bytes at a time is not worth a table.
fn crc32(data: &[u8]) -> u64 {
    let mut crc: u32 = u32::MAX;
    for byte in data {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    u64::from(!crc)
}

#[cfg(test)]
mod tests {
    use super::*;

    const REAL_SECRET: &str = "puddle-secret-API_KEY-00112233445566778899aabbccddeeff";
    const ENTROPY: &str = "abcdefghijklmnopqrstuvwxyzABCD";

    #[test]
    fn a_secret_name_of_64_bytes_is_the_longest_that_counts() {
        // Literal lengths: the limit is 64 bytes, whatever the constant says.
        let run = |len: usize| format!("{}-00112233445566778899aabbccddeeff", "N".repeat(len));
        assert_eq!(secret_name(run(64).as_bytes()).map(str::len), Some(64));
        assert_eq!(secret_name(run(65).as_bytes()), None);
        assert_eq!(secret_name(run(0).as_bytes()), None);
    }

    /// A token with the right checksum, built here so no source line looks like a real token.
    fn github_valid(prefix: &str) -> String {
        let checksum = String::from_utf8(checksum_of(ENTROPY.as_bytes()).to_vec()).unwrap();
        format!("{prefix}{ENTROPY}{checksum}")
    }

    /// The same token with the last checksum character changed.
    fn github_stale(prefix: &str) -> String {
        let mut text = github_valid(prefix);
        let last = text.pop().unwrap();
        text.push(if last == 'x' { 'y' } else { 'x' });
        text
    }

    fn none(_: &[u8]) -> bool {
        false
    }

    #[test]
    fn the_checksum_is_the_base62_crc_padded_with_zeros() {
        // 30 characters whose CRC-32 is 4246480780, "4dNndU" in base62 (cross-checked with
        // Python's zlib.crc32).
        assert_eq!(&checksum_of(ENTROPY.as_bytes()), b"4dNndU");
        // CRC-32 of nothing is 0: the text is all padding.
        assert_eq!(checksum_of(b"")[..], *b"000000");
    }

    #[test]
    fn crc32_matches_the_standard_check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn a_secret_shaped_value_is_flagged_with_its_name() {
        assert_eq!(
            find(format!("Bearer {REAL_SECRET}").as_bytes(), &none),
            ["stale-stand-in:secret:API_KEY"]
        );
    }

    #[test]
    fn a_registered_secret_is_not_flagged() {
        let known = |t: &[u8]| t == REAL_SECRET.as_bytes();
        assert_eq!(find(REAL_SECRET.as_bytes(), &known), Vec::<String>::new());
    }

    #[test]
    fn near_misses_are_not_flagged() {
        for text in [
            "puddle-secret-API_KEY-00112233445566778899aabbccddeef",
            "puddle-secret--00112233445566778899aabbccddeeff",
            "xpuddle-secret-A-00112233445566778899aabbccddeeff",
            "puddle-secret-A-00112233445566778899AABBCCDDEEFF",
            "puddle-secret-",
        ] {
            assert_eq!(find(text.as_bytes(), &none), Vec::<String>::new(), "{text}");
        }
    }

    #[test]
    fn a_github_token_is_flagged_only_when_its_checksum_is_wrong() {
        for prefix in ["gho_", "ghu_", "ghr_"] {
            assert_eq!(
                find(github_valid(prefix).as_bytes(), &none),
                Vec::<String>::new()
            );
            let stale = github_stale(prefix);
            assert_eq!(find(stale.as_bytes(), &none), [STALE_GITHUB_ID]);
        }
        let basic = format!("x-access-token:{}", github_stale("gho_"));
        assert_eq!(find(basic.as_bytes(), &none), [STALE_GITHUB_ID]);
        let held = github_stale("gho_");
        let known = |t: &[u8]| t == held.as_bytes();
        assert_eq!(find(held.as_bytes(), &known), Vec::<String>::new());
    }

    #[test]
    fn a_github_look_alike_of_another_length_or_kind_is_not_flagged() {
        let stale = github_stale("gho_");
        for text in [
            "gho_short".to_owned(),
            github_stale("ghp_"),
            format!("{stale}x"),
            format!("a{stale}"),
        ] {
            assert_eq!(find(text.as_bytes(), &none), Vec::<String>::new(), "{text}");
        }
    }
}
