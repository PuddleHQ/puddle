// SPDX-License-Identifier: AGPL-3.0-or-later
//! Password hashing: Argon2id, version 0x13, in the OWASP minimum configuration (19 MiB of
//! memory, 2 passes, 1 lane), a fresh 16-byte random salt per password, and the parameters kept
//! inside each stored hash (PHC string), so raising them later still verifies the old hashes.
//!
//! The work is memory-hard on purpose: it costs tens of milliseconds and 19 MiB per attempt, so
//! callers run it off the async threads and the sign-in limiter bounds how many run per minute.

use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use argon2::{Algorithm, Argon2, Params, Version};
use zeroize::Zeroizing;

/// The fewest characters a password has.
pub(crate) const MIN_PASSWORD_CHARS: usize = 12;
/// The most bytes a password has (long input is a cost lever for an attacker, not safety).
pub(crate) const MAX_PASSWORD_BYTES: usize = 256;

/// Memory cost in KiB.
const MEMORY_KIB: u32 = 19 * 1024;
/// Passes over the memory.
const PASSES: u32 = 2;
/// Parallel lanes.
const LANES: u32 = 1;
const SALT_BYTES: usize = 16;

/// Why a password was refused or could not be hashed.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PasswordError {
    /// The password does not meet the policy; the text says how.
    Weak(&'static str),
    /// The operating system gave no randomness, or the hash could not be computed.
    Failed(String),
}

/// Checks `password` against the policy: [`MIN_PASSWORD_CHARS`] to [`MAX_PASSWORD_BYTES`], and
/// not the account's own name.
pub(crate) fn check_policy(password: &str, name: &str) -> Result<(), PasswordError> {
    if password.chars().count() < MIN_PASSWORD_CHARS {
        return Err(PasswordError::Weak(
            "a password needs at least 12 characters",
        ));
    }
    if password.len() > MAX_PASSWORD_BYTES {
        return Err(PasswordError::Weak("a password has at most 256 bytes"));
    }
    if password.eq_ignore_ascii_case(name) {
        return Err(PasswordError::Weak("a password can't be the user name"));
    }
    Ok(())
}

fn argon() -> Result<Argon2<'static>, PasswordError> {
    let params = Params::new(MEMORY_KIB, PASSES, LANES, None)
        .map_err(|e| PasswordError::Failed(e.to_string()))?;
    Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, params))
}

/// The hash of `password` as a PHC string (`$argon2id$v=19$m=19456,t=2,p=1$<salt>$<hash>`).
pub(crate) fn hash(password: &str) -> Result<String, PasswordError> {
    let mut salt = [0u8; SALT_BYTES];
    getrandom::fill(&mut salt).map_err(|e| PasswordError::Failed(e.to_string()))?;
    argon()?
        .hash_password_with_salt(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|e| PasswordError::Failed(e.to_string()))
}

/// Whether `password` is the one `stored` (a PHC string) was made from. A stored value that does
/// not parse never matches.
pub(crate) fn verify(stored: &str, password: &str) -> bool {
    // Parameters come from the stored string, so old hashes keep verifying after a change here.
    Argon2::default()
        .verify_password(password.as_bytes(), stored)
        .is_ok()
}

/// A hash of a random password nobody knows, to check a wrong user name against: an unknown name
/// then costs the same time as a wrong password.
pub(crate) fn decoy() -> Result<Zeroizing<String>, PasswordError> {
    let mut noise = [0u8; 16];
    getrandom::fill(&mut noise).map_err(|e| PasswordError::Failed(e.to_string()))?;
    hash(&format!("{noise:?}")).map(Zeroizing::new)
}

#[cfg(test)]
mod tests;
