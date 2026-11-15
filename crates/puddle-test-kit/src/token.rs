// SPDX-License-Identifier: AGPL-3.0-or-later
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
mod tests;
