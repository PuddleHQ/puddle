// SPDX-License-Identifier: GPL-3.0-or-later
//! Whose an entry in the operating system's credential store is: its name says so.
//!
//! A pasted token or a secret's value is kept under a name Puddle makes. The owner's names are as
//! they always were (`tok-…`, `env-…`); another user's start with `u<id>-`. A request that names a
//! stored entry may name only one of the principal's own; anyone else's is refused the same way
//! whether it exists or not, so a name tells nobody what another user holds.

use puddle_secrets::{SourceSpec, StoredId};
use puddle_store::UserId;

use crate::error::ApiError;

/// The name of `base` for `user`: `base` for the owner, `u<id>-base` for anyone else.
pub(crate) fn name(user: UserId, base: &str) -> String {
    if user == UserId::OWNER {
        base.to_owned()
    } else {
        format!("u{user}-{base}")
    }
}

/// Why no fresh stored name could be made.
#[derive(Debug, thiserror::Error)]
pub(crate) enum StoredIdError {
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
pub(crate) fn fresh(user: UserId, kind: &str) -> Result<StoredId, StoredIdError> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(StoredIdError::Random)?;
    let base = bytes
        .iter()
        .fold(format!("{kind}-"), |id, byte| format!("{id}{byte:02x}"));
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
pub(crate) fn is_users(user: UserId, id: &StoredId) -> bool {
    user_of(id) == user
}

fn not_yours() -> ApiError {
    ApiError::invalid("not a stored credential id of yours")
}

/// Refuses a source that is not `user`'s to use: a stored entry of someone else, or, for anyone
/// but the owner, an account signed in on this computer (the GitHub CLI's, Git's credential
/// helper's): those are the operator's.
///
/// # Errors
/// 422 for another user's stored entry; 403 for a signed-in account of the computer.
pub(crate) fn check_source(user: UserId, source: &SourceSpec) -> Result<(), ApiError> {
    match source {
        SourceSpec::Stored { id, .. } if is_users(user, id) => Ok(()),
        SourceSpec::Stored { .. } => Err(not_yours()),
        _ if user == UserId::OWNER => Ok(()),
        _ => Err(crate::authz::Denial::OwnerOnly.into()),
    }
}

#[cfg(test)]
mod tests;
