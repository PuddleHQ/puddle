// SPDX-License-Identifier: AGPL-3.0-or-later
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
mod tests;
