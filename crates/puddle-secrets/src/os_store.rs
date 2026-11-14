// SPDX-License-Identifier: AGPL-3.0-or-later
//! The connection to the platform's credential store, owned here so it can be made again.
//!
//! The `keyring` crate's own set-up runs once per process and keeps its first answer, a failure
//! included: a computer whose Secret Service was locked or missing at start stays failed until
//! the process ends. `keyring-core` itself lets the default store be replaced at any time, so
//! this module builds the platform store through the platform crates and sets it as the default,
//! and [`reconnect_os_store`] does that again on demand.

use std::sync::{Mutex, PoisonError};

use crate::store::StoreError;

/// A connection that is made when first needed and again when asked, and remembers only whether
/// the last attempt worked. `init` replaces `keyring-core`'s default store.
struct Connection {
    init: fn() -> Result<(), StoreError>,
    ready: Mutex<bool>,
}

impl Connection {
    const fn new(init: fn() -> Result<(), StoreError>) -> Self {
        Self {
            init,
            ready: Mutex::new(false),
        }
    }

    /// Connects unless an earlier attempt worked. A failed attempt is tried again by the next
    /// call, so a store that was locked or missing at start is picked up once it is there.
    fn ensure(&self) -> Result<(), StoreError> {
        let mut ready = self.ready.lock().unwrap_or_else(PoisonError::into_inner);
        if !*ready {
            *ready = (self.init)().is_ok();
        }
        if *ready { Ok(()) } else { Err(StoreError) }
    }

    /// Connects again whether or not an earlier attempt worked, so a connection that went stale
    /// (a Secret Service that restarted) is replaced too.
    fn reconnect(&self) -> Result<(), StoreError> {
        let mut ready = self.ready.lock().unwrap_or_else(PoisonError::into_inner);
        *ready = (self.init)().is_ok();
        if *ready { Ok(()) } else { Err(StoreError) }
    }
}

static OS: Connection = Connection::new(connect_platform_store);

/// Makes the platform store `keyring-core`'s default.
fn connect_platform_store() -> Result<(), StoreError> {
    #[cfg(target_os = "macos")]
    let store = apple_native_keyring_store::keychain::Store::new().map_err(|_| StoreError)?;
    #[cfg(target_os = "windows")]
    let store = windows_native_keyring_store::Store::new().map_err(|_| StoreError)?;
    #[cfg(all(unix, not(target_os = "macos")))]
    let store = zbus_secret_service_keyring_store::Store::new().map_err(|_| StoreError)?;
    #[cfg(any(unix, windows))]
    {
        keyring_core::set_default_store(store);
        Ok(())
    }
    #[cfg(not(any(unix, windows)))]
    Err(StoreError)
}

/// Connects to the platform store unless that already worked; an error says nothing answers.
pub(crate) fn ensure_os_store() -> Result<(), StoreError> {
    OS.ensure()
}

/// Connects to the platform store again, replacing the connection from start-up. This is what
/// lets a Puddle that began with a locked or missing store use it after the user unlocked it,
/// without a restart. An error says nothing answers now.
///
/// # Errors
/// [`StoreError`] when the platform store does not answer.
pub fn reconnect_os_store() -> Result<(), StoreError> {
    OS.reconnect()
}

#[cfg(test)]
mod tests;
