// SPDX-License-Identifier: GPL-3.0-or-later
//! Made-up secret-shaped values, built at run time so no complete token is ever written in
//! source: a secret scanner then needs no allowlist entry per fixture, and a real-looking value
//! in a test cannot be mistaken for a leaked one.
//!
//! A token has the shape a classifier reads: the prefix, the body length, and for the kinds that
//! carry one, the checksum. The body comes from a `seed` (the same seed gives the same token),
//! so a test can name the part it must not find in a log with [`entropy`].

const BASE62: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// Characters of random-looking body ahead of the checksum in GitHub and npm tokens.
pub const CHECKSUMMED_ENTROPY_LEN: usize = 30;
const CHECKSUM_LEN: usize = 6;
const FINE_GRAINED_ID_LEN: usize = 22;
const FINE_GRAINED_SECRET_LEN: usize = 59;
const GITLAB_BODY_LEN: usize = 20;
const ANTHROPIC_BODY_LEN: usize = 93;
const OPENAI_BODY_LEN: usize = 120;
const AZURE_LEN: usize = 84;
const AZURE_MARK_AT: usize = 76;

/// A kind of token whose shape [`token`] builds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenKind {
    /// `ghp_`: GitHub personal access token (classic), with checksum.
    GithubPersonal,
    /// `gho_`: GitHub OAuth access token, with checksum.
    GithubOauth,
    /// `ghu_`: GitHub user-to-server token, with checksum.
    GithubUserToServer,
    /// `ghs_`: GitHub server-to-server token, with checksum.
    GithubServerToServer,
    /// `ghr_`: GitHub refresh token, with checksum.
    GithubRefresh,
    /// `github_pat_`: GitHub fine-grained token, by shape only (no published checksum).
    GithubFineGrained,
    /// `npm_`: npm access token, with checksum.
    Npm,
    /// `glpat-`: GitLab personal access token, by prefix and length (no published checksum).
    Gitlab,
    /// `sk-ant-api03-`: Anthropic API key, by prefix and length.
    Anthropic,
    /// `sk-proj-`: `OpenAI` project key, by prefix and length.
    OpenaiProject,
    /// The newer Azure DevOps personal access token: 84 base62 characters with `AZDO` at 76.
    /// It has no prefix.
    AzureDevops,
}

impl TokenKind {
    /// The kinds of GitHub token that carry a checksum.
    pub const GITHUB_CHECKSUMMED: [Self; 5] = [
        Self::GithubPersonal,
        Self::GithubOauth,
        Self::GithubUserToServer,
        Self::GithubServerToServer,
        Self::GithubRefresh,
    ];

    /// The text a token of this kind starts with.
    #[must_use]
    pub const fn prefix(self) -> &'static str {
        match self {
            Self::GithubPersonal => "ghp_",
            Self::GithubOauth => "gho_",
            Self::GithubUserToServer => "ghu_",
            Self::GithubServerToServer => "ghs_",
            Self::GithubRefresh => "ghr_",
            Self::GithubFineGrained => "github_pat_",
            Self::Npm => "npm_",
            Self::Gitlab => "glpat-",
            Self::Anthropic => "sk-ant-api03-",
            Self::OpenaiProject => "sk-proj-",
            Self::AzureDevops => "",
        }
    }
}

/// `len` base62 characters derived from `seed`: the same seed and length give the same text.
#[must_use]
pub fn entropy(seed: &str, len: usize) -> String {
    // FNV-1a of the seed starts a linear congruential generator; the top bits pick each character.
    let mut state: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in seed.bytes() {
        state ^= u64::from(byte);
        state = state.wrapping_mul(0x0100_0000_01b3);
    }
    (0..len)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let pick = usize::try_from((state >> 33) % 62).unwrap_or(0);
            char::from(BASE62.get(pick).copied().unwrap_or(b'0'))
        })
        .collect()
}

/// A token of `kind`, its body derived from `seed`.
#[must_use]
pub fn token(kind: TokenKind, seed: &str) -> String {
    let prefix = kind.prefix();
    match kind {
        TokenKind::GithubFineGrained => format!(
            "{prefix}{}_{}",
            entropy(seed, FINE_GRAINED_ID_LEN),
            entropy(&format!("{seed}/secret"), FINE_GRAINED_SECRET_LEN)
        ),
        TokenKind::Gitlab => format!("{prefix}{}", entropy(seed, GITLAB_BODY_LEN)),
        TokenKind::Anthropic => format!("{prefix}{}AA", entropy(seed, ANTHROPIC_BODY_LEN)),
        TokenKind::OpenaiProject => format!("{prefix}{}", entropy(seed, OPENAI_BODY_LEN)),
        TokenKind::AzureDevops => format!(
            "{}AZDO{}",
            entropy(seed, AZURE_MARK_AT),
            entropy(&format!("{seed}/tail"), AZURE_LEN - AZURE_MARK_AT - 4)
        ),
        _ => with_checksum(prefix, &entropy(seed, CHECKSUMMED_ENTROPY_LEN)),
    }
}

/// A made-up JSON Web Token: the header of an `HS256` token, `claims` (a JSON object as text),
/// and a signature of random-looking text derived from `seed`. Only the shape is real: the
/// signature verifies nothing.
#[must_use]
pub fn jwt(seed: &str, claims: &str) -> String {
    format!(
        "{}.{}.{}",
        base64url(br#"{"alg":"HS256","typ":"JWT"}"#),
        base64url(claims.as_bytes()),
        entropy(seed, 43)
    )
}

/// Base64 with the URL-safe alphabet and no padding.
fn base64url(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let mut block = [0u8; 3];
        for (slot, byte) in block.iter_mut().zip(chunk) {
            *slot = *byte;
        }
        let bits = u32::from(block[0]) << 16 | u32::from(block[1]) << 8 | u32::from(block[2]);
        for index in 0..=chunk.len() {
            let pick = usize::try_from((bits >> (18 - 6 * index)) & 0x3f).unwrap_or(0);
            out.push(char::from(ALPHABET.get(pick).copied().unwrap_or(b'A')));
        }
    }
    out
}

/// `prefix`, the 30 characters of `entropy`, then their checksum: for a test that needs a body
/// of its own (say, one with a character outside base62).
///
/// # Panics
/// If `entropy` is not [`CHECKSUMMED_ENTROPY_LEN`] bytes.
#[must_use]
pub fn with_checksum(prefix: &str, entropy: &str) -> String {
    assert_eq!(entropy.len(), CHECKSUMMED_ENTROPY_LEN, "entropy length");
    format!("{prefix}{entropy}{}", checksum_of(entropy.as_bytes()))
}

/// `token` with its last character changed, so a checksum no longer matches while the shape
/// stays.
#[must_use]
pub fn with_broken_checksum(token: &str) -> String {
    let mut text = token.to_owned();
    let last = text.pop().unwrap_or('0');
    text.push(if last == '0' { '1' } else { '0' });
    text
}

/// The six base62 characters that follow the entropy: its CRC-32, most significant digit first,
/// padded with zeros.
fn checksum_of(entropy: &[u8]) -> String {
    let mut value = u64::from(crc32(entropy));
    let mut digits = [b'0'; CHECKSUM_LEN];
    for slot in digits.iter_mut().rev() {
        let pick = usize::try_from(value % 62).unwrap_or(0);
        *slot = BASE62.get(pick).copied().unwrap_or(b'0');
        value /= 62;
    }
    digits.iter().copied().map(char::from).collect()
}

/// CRC-32 (IEEE, as zlib), bitwise.
fn crc32(data: &[u8]) -> u32 {
    let mut crc = u32::MAX;
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
    !crc
}

#[cfg(test)]
mod tests {
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
}
