// SPDX-License-Identifier: AGPL-3.0-or-later
//! The retry of the credential store check, for the first-run banner: the
//! [`CredentialStoreService`] trait the route calls and [`NoCredentialStore`] when none is wired.
//!
//! The check, the problem it raises and the reconnect live in the host; this crate only asks.

/// Checks the credential store again.
pub trait CredentialStoreService: Send + Sync {
    /// Connects to the credential store again, writes and deletes a throwaway value, and raises
    /// or clears the `credential-store` problem to match. `Some(true)` means the store works now;
    /// `None` means this build has no such check. It may wait on a locked store, so the route
    /// calls it off the async threads.
    fn retry(&self) -> Option<bool>;
}

/// The service when none is wired in: there is nothing to retry.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoCredentialStore;

impl CredentialStoreService for NoCredentialStore {
    fn retry(&self) -> Option<bool> {
        None
    }
}
