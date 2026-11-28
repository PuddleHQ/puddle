// SPDX-License-Identifier: AGPL-3.0-or-later
//! Which stored credentials a user may name in a request.

use puddle_secrets::SourceSpec;
use puddle_services::error::ApiError;
use puddle_services::stored_ids::is_users;
use puddle_store::UserId;

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
