// SPDX-License-Identifier: AGPL-3.0-or-later
//! The random secrets of a session: refresh tokens, access tokens and invite secrets. Each is 32
//! random bytes as 43 URL-safe characters. Only a SHA-256 digest of one is ever stored or looked
//! up, so the database (a backup, a stolen file) holds nothing that signs in.
//!
//! Access tokens live in memory only, 15 minutes each, and are lost at a restart: the page
//! refreshes silently from its cookie.

use std::collections::HashMap;
use std::sync::Mutex;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use puddle_store::DeviceId;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

/// How long an access token works.
pub(crate) const ACCESS_TTL_MS: u64 = 15 * 60_000;
/// Access tokens one device may hold at once (a page makes a new one every few minutes).
const PER_DEVICE: usize = 4;

/// A fresh secret: 32 random bytes as text.
pub(crate) fn random_secret() -> Result<Zeroizing<String>, getrandom::Error> {
    let mut bytes = Zeroizing::new([0u8; 32]);
    getrandom::fill(&mut *bytes)?;
    Ok(Zeroizing::new(URL_SAFE_NO_PAD.encode(*bytes)))
}

/// What is stored and looked up in place of a secret.
pub(crate) fn digest(secret: &str) -> [u8; 32] {
    Sha256::digest(secret.as_bytes()).into()
}

#[derive(Debug, Clone, Copy)]
struct Access {
    device: DeviceId,
    expires_at: u64,
    issued_at: u64,
}

/// The live access tokens, by digest.
#[derive(Debug, Default)]
pub(crate) struct AccessTokens {
    live: Mutex<HashMap<[u8; 32], Access>>,
}

impl AccessTokens {
    /// A new access token for `device`, and when it stops working.
    pub(crate) fn issue(
        &self,
        device: DeviceId,
        now: u64,
    ) -> Result<(Zeroizing<String>, u64), getrandom::Error> {
        let token = random_secret()?;
        let expires_at = now.saturating_add(ACCESS_TTL_MS);
        let mut live = self
            .live
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        live.retain(|_, a| a.expires_at > now);
        // Keep the newest few of this device's tokens; the page only ever uses the latest.
        let mut mine: Vec<([u8; 32], u64)> = live
            .iter()
            .filter(|(_, a)| a.device == device)
            .map(|(d, a)| (*d, a.issued_at))
            .collect();
        if mine.len() >= PER_DEVICE {
            mine.sort_by_key(|&(_, issued)| issued);
            for (old, _) in mine.iter().take(mine.len() + 1 - PER_DEVICE) {
                live.remove(old);
            }
        }
        live.insert(
            digest(&token),
            Access {
                device,
                expires_at,
                issued_at: now,
            },
        );
        Ok((token, expires_at))
    }

    /// The device `token` belongs to, while it has not run out.
    pub(crate) fn device_of(&self, token: &str, now: u64) -> Option<DeviceId> {
        let live = self
            .live
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        live.get(&digest(token))
            .filter(|a| a.expires_at > now)
            .map(|a| a.device)
    }

    /// Ends every token of `device`.
    pub(crate) fn revoke(&self, device: DeviceId) {
        self.live
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|_, a| a.device != device);
    }
}

#[cfg(test)]
mod tests;
