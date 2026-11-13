// SPDX-License-Identifier: GPL-3.0-or-later
//! `puddle user reset <name>`: the server's own way to give a user back their way in. It takes
//! the password away, signs every device of that user out, and makes a one-time invite; it never
//! sets or shows a password. Like `puddle backup`, it works on the database file while the host
//! runs (the file allows several processes) and never takes the data-folder lock.

use std::sync::Arc;

use puddle_api::{ResetError, ResetInvite, reset_account};
use puddle_store::{Clock as _, Limits, Store, SystemClock};

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
}

/// Resets `name` in the data folder `paths` names, and returns the invite's secret.
///
/// # Errors
///
/// [`AccountError`] as its variants say.
pub fn reset_user(paths: &HostPaths, name: &str) -> Result<ResetInvite, AccountError> {
    let db = paths.store();
    if !db.is_file() {
        return Err(AccountError::NoDatabase(db));
    }
    let clock = Arc::new(SystemClock);
    let store = Store::open(&db, clock.clone(), Limits::default())?;
    let Some(secrets) = store.take_account_secrets() else {
        // A fresh handle always has them; reaching here means this function was changed.
        return Err(ResetError::SecretsTaken.into());
    };
    Ok(reset_account(&store, &secrets, name, clock.now_ms())?)
}

#[cfg(test)]
mod tests;
