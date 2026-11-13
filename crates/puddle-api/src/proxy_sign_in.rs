// SPDX-License-Identifier: AGPL-3.0-or-later
//! The sign-ins Puddle holds for company proxies that ask for a user name and password: the
//! [`ProxySignInService`] trait the routes and the network-health report call, [`NoProxySignIns`]
//! for a build without it, and [`FakeProxySignIns`] for tests and the UI fixture.
//!
//! The values live in the operating system's credential store (see the proxy spec); nothing here
//! returns a password.

use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};

use puddle_upstream::ProxyAddr;

use crate::wire::StoredSignIn;

/// Why a sign-in could not be stored or removed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ProxySignInError {
    /// The user name is empty, too long, or has a colon or a line break in it.
    #[error("the user name is empty, too long, or has a colon or a line break in it")]
    BadUser,
    /// The password is empty or has a line break in it.
    #[error("the password is empty or has a line break in it")]
    BadPassword,
    /// The operating system's credential store did not do what was asked (it may be locked).
    #[error("the operating system's credential store did not answer; unlock it and try again")]
    Store,
    /// The service is not wired in this build (503).
    #[error("{0}")]
    Unavailable(String),
}

/// What the proxy sign-in screens ask of the host. The methods may block on the credential store,
/// so callers run them off the async threads.
pub trait ProxySignInService: Send + Sync {
    /// The stored sign-ins, sorted by proxy; never a password.
    fn list(&self) -> Vec<StoredSignIn>;

    /// Stores `user` and `password` for `proxy`, replacing an earlier sign-in.
    ///
    /// # Errors
    /// [`ProxySignInError`] for a value Basic cannot carry or a store that failed; nothing changes
    /// then.
    fn set(&self, proxy: &ProxyAddr, user: &str, password: &str) -> Result<(), ProxySignInError>;

    /// Forgets the sign-in for `proxy`; one that is not stored is fine.
    ///
    /// # Errors
    /// [`ProxySignInError::Store`] when the store failed.
    fn remove(&self, proxy: &ProxyAddr) -> Result<(), ProxySignInError>;
}

/// The service when none is wired in: nothing is stored and nothing can be.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoProxySignIns;

impl ProxySignInService for NoProxySignIns {
    fn list(&self) -> Vec<StoredSignIn> {
        Vec::new()
    }

    fn set(&self, _: &ProxyAddr, _: &str, _: &str) -> Result<(), ProxySignInError> {
        Err(unavailable())
    }

    fn remove(&self, _: &ProxyAddr) -> Result<(), ProxySignInError> {
        Err(unavailable())
    }
}

fn unavailable() -> ProxySignInError {
    ProxySignInError::Unavailable("proxy sign-in is not available in this build yet".into())
}

/// An in-memory service for tests and the UI fixture. It applies the same value rules as the
/// host and keeps the password, which a test reads back through [`FakeProxySignIns::password`].
#[derive(Debug, Default)]
pub struct FakeProxySignIns {
    state: Mutex<BTreeMap<ProxyAddr, (String, String)>>,
    failing: Mutex<bool>,
}

impl FakeProxySignIns {
    /// A fake with nothing stored.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Makes every later `set` and `remove` fail as a locked credential store would.
    pub fn fail_store(&self, failing: bool) {
        *self.failing.lock().unwrap_or_else(PoisonError::into_inner) = failing;
    }

    /// The password stored for `proxy`, for a test that checks it was kept.
    #[must_use]
    pub fn password(&self, proxy: &ProxyAddr) -> Option<String> {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(proxy)
            .map(|(_, password)| password.clone())
    }

    fn store_down(&self) -> bool {
        *self.failing.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl ProxySignInService for FakeProxySignIns {
    fn list(&self) -> Vec<StoredSignIn> {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(proxy, (user, _))| StoredSignIn {
                proxy: proxy.to_string(),
                user: user.clone(),
            })
            .collect()
    }

    fn set(&self, proxy: &ProxyAddr, user: &str, password: &str) -> Result<(), ProxySignInError> {
        if user.is_empty() || user.len() > 256 || user.contains([':', '\n', '\r']) {
            return Err(ProxySignInError::BadUser);
        }
        if password.is_empty() || password.contains(['\n', '\r']) {
            return Err(ProxySignInError::BadPassword);
        }
        if self.store_down() {
            return Err(ProxySignInError::Store);
        }
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(proxy.clone(), (user.to_owned(), password.to_owned()));
        Ok(())
    }

    fn remove(&self, proxy: &ProxyAddr) -> Result<(), ProxySignInError> {
        if self.store_down() {
            return Err(ProxySignInError::Store);
        }
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(proxy);
        Ok(())
    }
}
