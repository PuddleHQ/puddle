// SPDX-License-Identifier: AGPL-3.0-or-later
//! `puddle user add|list|remove|reset`: the server's own way to manage accounts. `add` and `reset`
//! make a one-time invite (reset also takes the password away and signs every device out); none
//! of them ever sets or shows a password. `remove` starts the deletion the running server
//! finishes. Like `puddle backup`, it works on the database file while the host
//! runs (the file allows several processes) and never takes the data-folder lock.

use std::sync::Arc;

use puddle_api::{ResetError, ResetInvite, reset_account, reset_user_account};
use puddle_store::{
    AccountSecrets, Clock as _, Limits, Role, Store, SystemClock, UserId, UserName,
};

use crate::HostPaths;

/// Why a reset was not made.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AccountError {
    /// There is no database to work on.
    #[error("no database at {0}; is this the right data folder?")]
    NoDatabase(std::path::PathBuf),
    /// The database could not be opened.
    #[error("cannot open the database: {0}")]
    Store(#[from] puddle_store::StoreError),
    /// The reset itself failed.
    #[error(transparent)]
    Reset(#[from] ResetError),
    /// There is no user with this name.
    #[error("no user named {0}")]
    UnknownUser(String),
}

/// Where an account stands, as `puddle user list` shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserState {
    /// Made by the owner; the person has not chosen a password yet.
    Invited,
    /// The person has a password.
    Active,
    /// The deletion has begun and the server is removing what is left.
    Deleting,
}

/// One line of `puddle user list`: counts and dates, never what is inside an account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserRow {
    /// The user name.
    pub name: String,
    /// Whether this is the owner.
    pub owner: bool,
    /// Where the account stands.
    pub state: UserState,
    /// Browsers signed in that still work.
    pub devices: u32,
    /// When they last signed in, epoch ms.
    pub last_sign_in: Option<u64>,
}

/// The database of the data folder `paths` names, with the one-time secrets handle.
fn open(paths: &HostPaths) -> Result<(Store, AccountSecrets, u64), AccountError> {
    let db = paths.store();
    if !db.is_file() {
        return Err(AccountError::NoDatabase(db));
    }
    let clock = Arc::new(SystemClock);
    let store = Store::open(&db, clock.clone(), Limits::default())?;
    // A fresh handle always has them; a miss here means this function was changed.
    let secrets = store
        .take_account_secrets()
        .ok_or(ResetError::SecretsTaken)?;
    Ok((store, secrets, clock.now_ms()))
}

/// Resets `name` in the data folder `paths` names, and returns the invite's secret.
///
/// # Errors
///
/// [`AccountError`] as its variants say.
pub fn reset_user(paths: &HostPaths, name: &str) -> Result<ResetInvite, AccountError> {
    let (store, secrets, now) = open(paths)?;
    Ok(reset_account(&store, &secrets, name, now)?)
}

/// Makes the user `name` in the invited state and returns their invite's secret. The owner
/// is the one who did it, for the admin audit.
///
/// # Errors
///
/// [`AccountError`] as its variants say; a name that is refused or taken is a
/// [`puddle_store::StoreError::UserName`].
pub fn add_user(paths: &HostPaths, name: &str) -> Result<ResetInvite, AccountError> {
    let (store, secrets, now) = open(paths)?;
    let name = UserName::new(name)?;
    let user = store.admin_add_user(UserId::OWNER, &name)?;
    Ok(reset_user_account(&store, &secrets, user.id, now)?)
}

/// Every user, the owner first.
///
/// # Errors
///
/// [`AccountError`] as its variants say.
pub fn list_users(paths: &HostPaths) -> Result<Vec<UserRow>, AccountError> {
    let (store, secrets, _) = open(paths)?;
    store
        .user_facts()?
        .into_iter()
        .map(|facts| {
            let state = if facts.deleting_since.is_some() {
                UserState::Deleting
            } else if secrets.has_password(facts.user.id)? {
                UserState::Active
            } else {
                UserState::Invited
            };
            Ok(UserRow {
                owner: facts.user.role == Role::Owner,
                name: facts.user.name,
                state,
                devices: facts.devices,
                last_sign_in: facts.last_sign_in,
            })
        })
        .collect::<Result<_, puddle_store::StoreError>>()
        .map_err(Into::into)
}

/// Starts deleting `name`: they can no longer sign in, and the running server stops and removes
/// their workspaces and then the user (at its next start when it is not running). Irreversible.
///
/// # Errors
///
/// [`AccountError`] as its variants say; the owner is [`puddle_store::StoreError::OwnerKept`].
pub fn remove_user(paths: &HostPaths, name: &str) -> Result<(), AccountError> {
    let (store, secrets, _) = open(paths)?;
    let user = store
        .user_named(name)?
        .ok_or_else(|| AccountError::UnknownUser(name.to_owned()))?;
    store.begin_user_delete(UserId::OWNER, user.id)?;
    secrets.clear_password(user.id)?;
    secrets.clear_invite(user.id)?;
    store.revoke_devices_of(user.id, None)?;
    Ok(())
}

#[cfg(test)]
mod tests;
