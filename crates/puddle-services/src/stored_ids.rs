// SPDX-License-Identifier: AGPL-3.0-or-later
//! Whose an entry in the operating system's credential store is: its name says so.
//!
//! A pasted token or a secret's value is kept under a name Puddle makes. The owner's names are as
//! they always were (`tok-…`, `env-…`); another user's start with `u<id>-`. A request that names a
//! stored entry may name only one of the principal's own; anyone else's is refused the same way
//! whether it exists or not, so a name tells nobody what another user holds.

use puddle_secrets::StoredId;
use puddle_store::UserId;

/// The name of `base` for `user`: `base` for the owner, `u<id>-base` for anyone else.
#[must_use]
pub fn name(user: UserId, base: &str) -> String {
    if user == UserId::OWNER {
        base.to_owned()
    } else {
        format!("u{user}-{base}")
    }
}

/// Why no fresh stored name could be made.
#[derive(Debug, thiserror::Error)]
pub enum StoredIdError {
    /// The system's random source failed.
    #[error("no randomness: {0}")]
    Random(getrandom::Error),
    /// The name is not a valid id (a bug).
    #[error(transparent)]
    Invalid(#[from] puddle_secrets::NameError),
}

/// A fresh name for `user`: `kind`, `-` and 128 random bits.
///
/// # Errors
/// The system's random source failed, or the name is not a valid id (a bug).
pub fn fresh(user: UserId, kind: &str) -> Result<StoredId, StoredIdError> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(StoredIdError::Random)?;
    let base = format!("{kind}-{}", puddle_types::hex_lower(&bytes));
    Ok(StoredId::new(name(user, &base))?)
}

/// The user an entry's name says it belongs to.
fn user_of(id: &StoredId) -> UserId {
    id.as_str()
        .strip_prefix('u')
        .and_then(|rest| rest.split_once('-'))
        .filter(|(digits, _)| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|(digits, _)| digits.parse::<i64>().ok())
        .map_or(UserId::OWNER, UserId::from_stored)
}

/// Whether the entry `id` is `user`'s.
#[must_use]
pub fn is_users(user: UserId, id: &StoredId) -> bool {
    user_of(id) == user
}

#[cfg(test)]
mod tests;
