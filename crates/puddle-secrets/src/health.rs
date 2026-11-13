// SPDX-License-Identifier: GPL-3.0-or-later
//! Whether the credential store takes a write, and what to tell the user when it does not.
//!
//! [`keyring_available`] only asks whether the store's service answers. A locked Secret Service
//! answers and then refuses every write, and on a host with no desktop session nothing can prompt
//! for the unlock. So the check here writes a throwaway value and deletes it again.

use crate::keyring_store::{KeyringStore, keyring_available};
use crate::name::StoredId;
use crate::secret::Secret;
use crate::store::SecretStore;

/// The service the throwaway entry of [`probe_os_store`] is filed under, apart from the one that
/// holds Puddle's own values.
const PROBE_SERVICE: &str = "puddle-startup-probe";

/// How a write to a credential store came out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreHealth {
    /// A value was written and deleted again.
    Working,
    /// The write failed and nothing answers where the store should be: no Secret Service
    /// provider on Linux.
    NoProvider,
    /// The write failed although the store's service answers: a locked keyring that nothing can
    /// prompt to unlock.
    Locked,
}

/// What to tell the user about a store that does not work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoreAdvice {
    /// One line: what is wrong.
    pub title: &'static str,
    /// What Puddle does about it and the fix. It says to restart Puddle, because a store that
    /// failed to start once stays failed for the life of the process.
    pub fix: &'static str,
}

impl StoreHealth {
    /// The text for a store that does not work, worded for Linux when `linux` is set and for the
    /// other platforms otherwise; `None` for a working store.
    #[must_use]
    pub fn advice(self, linux: bool) -> Option<StoreAdvice> {
        match (self, linux) {
            (Self::Working, _) => None,
            (Self::NoProvider, true) => Some(StoreAdvice {
                title: "Puddle cannot save secrets: no credential store is running",
                fix: "Puddle keeps tokens and secrets in the Secret Service (gnome-keyring or \
                      KWallet), and nothing on this computer provides one, so saving a secret or \
                      a sign-in fails. On a headless Linux host install gnome-keyring \
                      (`apt install gnome-keyring` on Debian and Ubuntu), unlock it with \
                      `printf %s \"<password>\" | gnome-keyring-daemon --daemonize --login \
                      --components=secrets`, then restart Puddle. Puddle checks the store only \
                      when it starts and cannot recover while it runs.",
            }),
            (Self::Locked, true) => Some(StoreAdvice {
                title: "Puddle cannot save secrets: the credential store is locked",
                fix: "The Secret Service answers but refuses writes, and nothing on this \
                      computer can ask you to unlock it. On a headless Linux host unlock it with \
                      `printf %s \"<password>\" | gnome-keyring-daemon --daemonize --login \
                      --components=secrets` and restart Puddle. This is needed again after every \
                      reboot. Puddle checks the store only when it starts and cannot recover \
                      while it runs.",
            }),
            (Self::NoProvider, false) => Some(StoreAdvice {
                title: "Puddle cannot save secrets: the credential store is not available",
                fix: "The operating system's credential store does not answer, so saving a \
                      secret or a sign-in fails. Check that it is running, then restart Puddle: \
                      Puddle checks the store only when it starts and cannot recover while it \
                      runs.",
            }),
            (Self::Locked, false) => Some(StoreAdvice {
                title: "Puddle cannot save secrets: the credential store is locked",
                fix: "The operating system's credential store refuses writes. Unlock it (sign in \
                      to the account that runs Puddle), then restart Puddle: Puddle checks the \
                      store only when it starts and cannot recover while it runs.",
            }),
        }
    }
}

/// Writes a throwaway value to `store` and deletes it. When the write fails, `service_answers`
/// tells a missing store (`NoProvider`) from a locked one (`Locked`); it runs only then.
pub fn probe_store(store: &dyn SecretStore, service_answers: impl FnOnce() -> bool) -> StoreHealth {
    // The process id keeps two Puddle processes (the service and `puddle doctor`) from deleting
    // each other's entry.
    probe_entry(
        store,
        format!("startup-probe-{}", std::process::id()),
        service_answers,
    )
}

/// [`probe_store`] with the entry name given. A name the store's id rules refuse cannot be
/// written, which reads as a refusing store.
fn probe_entry(
    store: &dyn SecretStore,
    name: String,
    service_answers: impl FnOnce() -> bool,
) -> StoreHealth {
    let Ok(id) = StoredId::new(name) else {
        return StoreHealth::Locked;
    };
    if store.set(&id, &Secret::new("probe".to_owned())).is_err() {
        return if service_answers() {
            StoreHealth::Locked
        } else {
            StoreHealth::NoProvider
        };
    }
    if store.delete(&id).is_err() {
        tracing::warn!("the credential store took the startup probe but did not delete it");
    }
    StoreHealth::Working
}

/// [`probe_store`] against the platform's credential store, under a service of its own so the
/// entry never sits among Puddle's values.
#[must_use]
pub fn probe_os_store() -> StoreHealth {
    probe_store(&KeyringStore::named(PROBE_SERVICE), keyring_available)
}

#[cfg(test)]
mod tests;
