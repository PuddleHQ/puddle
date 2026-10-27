// SPDX-License-Identifier: GPL-3.0-or-later
//! What Microsoft's `code serve-web` adds around its server and a direct start loses: the
//! secret-storage key. The workbench keeps secrets in memory unless the cookie
//! `vscode-secret-key-path` exists; it then POSTs to the path in it and combines the 32 bytes it
//! gets with a key of its own. Same scheme as the launcher's: a server half kept on the host, a
//! client half in an `HttpOnly` cookie, key = SHA-256(server half ‖ client half).
//!
//! If Microsoft ever changes the contract this fails soft: secrets fall back to memory.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use http::{HeaderMap, HeaderValue, header};
use sha2::{Digest, Sha256};

use crate::keys::HALF_LEN;

/// The path the key is minted at (the front answers it; the server never sees it).
pub const MINT_PATH: &str = "/_puddle/mint-key";

/// The cookie the workbench reads to find the mint path.
pub const KEY_PATH_COOKIE: &str = "vscode-secret-key-path";

/// The cookie that holds the client half.
pub const HALF_COOKIE: &str = "puddle-secret-half";

/// A month, refreshed on every response.
const HALF_MAX_AGE: u32 = 30 * 24 * 60 * 60;

/// The client half in `cookies` (the request's `Cookie` headers), if it is a valid one.
#[must_use]
pub fn client_half(headers: &HeaderMap) -> Option<[u8; HALF_LEN]> {
    let value = cookie_pairs(headers)
        .find(|(name, _)| *name == HALF_COOKIE)?
        .1;
    let bytes = URL_SAFE_NO_PAD.decode(value).ok()?;
    <[u8; HALF_LEN]>::try_from(bytes).ok()
}

/// A new client half.
///
/// # Errors
///
/// The system has no randomness.
pub fn new_client_half() -> std::io::Result<[u8; HALF_LEN]> {
    let mut half = [0_u8; HALF_LEN];
    getrandom::fill(&mut half).map_err(|e| std::io::Error::other(format!("no randomness: {e}")))?;
    Ok(half)
}

/// The key the workbench is given: SHA-256 of the server half and the client half.
#[must_use]
pub fn key(server: &[u8; HALF_LEN], client: &[u8; HALF_LEN]) -> [u8; 32] {
    Sha256::new()
        .chain_update(server)
        .chain_update(client)
        .finalize()
        .into()
}

/// The two `Set-Cookie` values for a response: where the key is minted, and the client half.
#[must_use]
pub fn set_cookies(client: &[u8; HALF_LEN]) -> [HeaderValue; 2] {
    let path = format!("{KEY_PATH_COOKIE}={MINT_PATH}; SameSite=Strict; Path=/");
    let half = format!(
        "{HALF_COOKIE}={}; SameSite=Strict; HttpOnly; Max-Age={HALF_MAX_AGE}; Path=/",
        URL_SAFE_NO_PAD.encode(client)
    );
    // Both are built from constants and base64url text, which are valid header values.
    [
        HeaderValue::from_str(&path).unwrap_or_else(|_| HeaderValue::from_static("")),
        HeaderValue::from_str(&half).unwrap_or_else(|_| HeaderValue::from_static("")),
    ]
}

/// The `name=value` pairs of every `Cookie` header.
fn cookie_pairs(headers: &HeaderMap) -> impl Iterator<Item = (&str, &str)> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|line| line.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
}

/// Takes the client half out of a request's cookies, so the guest's server never sees the half it
/// has no use for. Other cookies are kept as they were; a request with no other cookie loses the
/// header.
pub fn strip_client_half(headers: &mut HeaderMap) {
    if !headers.contains_key(header::COOKIE) {
        return;
    }
    let kept: Vec<String> = headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|line| line.split(';'))
        .map(str::trim)
        .filter(|pair| !pair.is_empty() && pair.split('=').next() != Some(HALF_COOKIE))
        .map(str::to_owned)
        .collect();
    headers.remove(header::COOKIE);
    if let (false, Ok(value)) = (kept.is_empty(), HeaderValue::from_str(&kept.join("; "))) {
        headers.insert(header::COOKIE, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(cookies: &[&str]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for c in cookies {
            h.append(header::COOKIE, HeaderValue::from_str(c).unwrap());
        }
        h
    }

    #[test]
    fn the_key_is_sha256_of_the_two_halves() {
        let key = key(&[1; 32], &[2; 32]);
        let mut both = vec![1_u8; 32];
        both.extend([2_u8; 32]);
        assert_eq!(key.to_vec(), Sha256::digest(&both).to_vec());
        assert_ne!(key, super::key(&[2; 32], &[1; 32]));
    }

    #[test]
    fn the_client_half_is_read_from_its_cookie_among_others() {
        let half = [7_u8; 32];
        let cookie = format!("a=b; {HALF_COOKIE}={}; c=d", URL_SAFE_NO_PAD.encode(half));
        assert_eq!(client_half(&headers(&[&cookie])), Some(half));
        assert_eq!(client_half(&headers(&["a=b"])), None);
        assert_eq!(client_half(&headers(&[])), None);
    }

    #[test]
    fn a_half_of_the_wrong_length_or_alphabet_is_not_one() {
        for bad in [
            "puddle-secret-half=AAAA",
            "puddle-secret-half=!!!",
            "puddle-secret-half=",
        ] {
            assert_eq!(client_half(&headers(&[bad])), None, "{bad}");
        }
    }

    #[test]
    fn the_set_cookies_point_at_the_mint_path_and_hide_the_half_from_scripts() {
        let [path, half] = set_cookies(&[9; 32]);
        assert_eq!(
            path.to_str().unwrap(),
            "vscode-secret-key-path=/_puddle/mint-key; SameSite=Strict; Path=/"
        );
        let half = half.to_str().unwrap();
        assert!(half.starts_with("puddle-secret-half="), "{half}");
        assert!(
            half.contains("HttpOnly") && half.contains("SameSite=Strict"),
            "{half}"
        );
        assert_eq!(
            client_half(&headers(&[half.split(';').next().unwrap()])),
            Some([9; 32])
        );
    }

    #[test]
    fn stripping_removes_only_the_half() {
        let mut h = headers(&["a=b; puddle-secret-half=xyz", "c=d"]);
        strip_client_half(&mut h);
        assert_eq!(h.get(header::COOKIE).unwrap(), "a=b; c=d");
        let mut only = headers(&["puddle-secret-half=xyz"]);
        strip_client_half(&mut only);
        assert!(!only.contains_key(header::COOKIE));
        let mut none = headers(&[]);
        strip_client_half(&mut none);
        assert!(none.is_empty());
    }
}
