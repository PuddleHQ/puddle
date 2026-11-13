// SPDX-License-Identifier: AGPL-3.0-or-later
//! The host's side of the proxy sign-ins: the service the API stores, lists and removes them
//! through. The values live in [`ProxyPasswords`]; a change also makes the chain forget the Basic
//! header it remembered from the old sign-in.

use std::sync::Arc;

use puddle_api::wire::StoredSignIn;
use puddle_api::{ProxySignInError, ProxySignInService};
use puddle_upstream::{Chain, ProxyAddr};

use crate::{ProxyPasswordError, ProxyPasswords};

/// The stored sign-ins of this host.
pub(crate) struct HostProxySignIns {
    passwords: Arc<ProxyPasswords>,
    chain: Arc<Chain>,
}

impl HostProxySignIns {
    pub(crate) fn new(passwords: Arc<ProxyPasswords>, chain: Arc<Chain>) -> Self {
        Self { passwords, chain }
    }
}

impl From<ProxyPasswordError> for ProxySignInError {
    fn from(err: ProxyPasswordError) -> Self {
        match err {
            ProxyPasswordError::BadUser => Self::BadUser,
            ProxyPasswordError::BadPassword => Self::BadPassword,
            _ => Self::Store,
        }
    }
}

impl ProxySignInService for HostProxySignIns {
    fn list(&self) -> Vec<StoredSignIn> {
        self.passwords
            .proxies()
            .into_iter()
            .map(|(proxy, user)| StoredSignIn {
                proxy: proxy.to_string(),
                user,
            })
            .collect()
    }

    fn set(&self, proxy: &ProxyAddr, user: &str, password: &str) -> Result<(), ProxySignInError> {
        self.passwords.set(proxy, user, password)?;
        self.chain.forget_basic_headers();
        Ok(())
    }

    fn remove(&self, proxy: &ProxyAddr) -> Result<(), ProxySignInError> {
        self.passwords.remove(proxy)?;
        self.chain.forget_basic_headers();
        Ok(())
    }
}

#[cfg(test)]
mod tests;
