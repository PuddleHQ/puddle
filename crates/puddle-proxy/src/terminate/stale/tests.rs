// SPDX-License-Identifier: AGPL-3.0-or-later
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
