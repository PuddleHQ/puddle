// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn the_crc_is_the_standard_check_value() {
    assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
}

#[test]
fn a_checksum_is_six_base62_characters() {
    let sum = checksum_of(entropy("x", CHECKSUMMED_ENTROPY_LEN).as_bytes());
    assert_eq!(sum.len(), CHECKSUM_LEN);
    assert!(sum.bytes().all(|b| b.is_ascii_alphanumeric()), "{sum}");
}

#[test]
fn a_seed_gives_the_same_token_and_another_seed_another() {
    let kind = TokenKind::GithubPersonal;
    assert_eq!(token(kind, "a"), token(kind, "a"));
    assert_ne!(token(kind, "a"), token(kind, "b"));
}

#[test]
fn each_kind_has_its_prefix_and_length() {
    let lengths = [
        (TokenKind::GithubPersonal, 4 + 36),
        (TokenKind::GithubRefresh, 4 + 36),
        (TokenKind::Npm, 4 + 36),
        (TokenKind::GithubFineGrained, 11 + 22 + 1 + 59),
        (TokenKind::Gitlab, 6 + 20),
        (TokenKind::Anthropic, 13 + 93 + 2),
        (TokenKind::OpenaiProject, 8 + 120),
        (TokenKind::AzureDevops, 84),
    ];
    for (kind, len) in lengths {
        let made = token(kind, "len");
        assert!(made.starts_with(kind.prefix()), "{made}");
        assert_eq!(made.len(), len, "{made}");
    }
}

#[test]
fn an_azure_devops_token_carries_its_mark_at_76() {
    let made = token(TokenKind::AzureDevops, "az");
    assert_eq!(made.get(76..80), Some("AZDO"));
    assert!(made.bytes().all(|b| b.is_ascii_alphanumeric()), "{made}");
}

#[test]
fn a_jwt_has_three_segments_and_the_claims_it_was_given() {
    let made = jwt("j", r#"{"sub":"a"}"#);
    let parts: Vec<&str> = made.split('.').collect();
    assert_eq!(parts.len(), 3, "{made}");
    assert_eq!(parts[0], "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9");
    assert_eq!(parts[1], "eyJzdWIiOiJhIn0");
    assert_eq!(parts[2].len(), 43);
}

#[test]
fn base64url_matches_the_standard_vectors() {
    for (plain, encoded) in [
        ("", ""),
        ("f", "Zg"),
        ("fo", "Zm8"),
        ("foo", "Zm9v"),
        ("foob", "Zm9vYg"),
        ("fooba", "Zm9vYmE"),
        ("foobar", "Zm9vYmFy"),
    ] {
        assert_eq!(base64url(plain.as_bytes()), encoded, "{plain:?}");
    }
    assert_eq!(base64url(&[0xfb, 0xff]), "-_8");
}

#[test]
fn a_broken_checksum_differs_only_in_the_last_character() {
    let good = token(TokenKind::Npm, "b");
    let bad = with_broken_checksum(&good);
    assert_eq!(good.len(), bad.len());
    assert_ne!(good, bad);
    assert_eq!(good.get(..good.len() - 1), bad.get(..bad.len() - 1));
}
