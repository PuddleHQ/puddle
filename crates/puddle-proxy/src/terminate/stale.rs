// SPDX-License-Identifier: AGPL-3.0-or-later
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
pub(crate) fn has_valid_checksum(body: &[u8]) -> bool {
    body.get(..GITHUB_ENTROPY)
        .zip(body.get(GITHUB_ENTROPY..))
        .is_some_and(|(entropy, checksum)| checksum_of(entropy) == checksum)
}

/// The six base62 characters that follow `entropy` in a real token.
pub(crate) fn checksum_of(entropy: &[u8]) -> [u8; GITHUB_CHECKSUM] {
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
mod tests;
