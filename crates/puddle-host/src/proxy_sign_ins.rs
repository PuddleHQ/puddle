// SPDX-License-Identifier: AGPL-3.0-or-later
//! The host's side of the proxy sign-ins: the service the API stores, lists and removes them
//! through. The values live in [`ProxyPasswords`]; a change also makes the chain forget the Basic
//! header it remembered from the old sign-in.

use std::sync::Arc;

use puddle_api::wire::StoredSignIn;
use puddle_api::{ProxySignInError, ProxySignInService};
use puddle_types::{Problem, Problems};
use puddle_upstream::{Chain, ProxyAddr};

use crate::{ProxyPasswordError, ProxyPasswords};

/// The stored sign-ins of this host.
pub(crate) struct HostProxySignIns {
    passwords: Arc<ProxyPasswords>,
    chain: Arc<Chain>,
    problems: Arc<Problems>,
}

/// Raises the problem that names the proxies whose sign-in the credential store lost, or ends it
/// when there is none.
pub(crate) fn report_lost(passwords: &ProxyPasswords, problems: &Problems) {
    const LOST: &str = "proxy-sign-in-lost";
    let lost = passwords.lost();
    if lost.is_empty() {
        problems.clear(LOST);
        return;
    }
    let names = lost
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    problems.raise(Problem::new(
        LOST,
        "Puddle lost a company proxy sign-in",
        format!(
            "The credential store no longer holds the sign-in for {names} (a reset or a new vault). Enter it again on the proxy sign-in page, or remove it there."
        ),
    ));
}

impl HostProxySignIns {
    pub(crate) fn new(
        passwords: Arc<ProxyPasswords>,
        chain: Arc<Chain>,
        problems: Arc<Problems>,
    ) -> Self {
        Self {
            passwords,
            chain,
            problems,
        }
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
        report_lost(&self.passwords, &self.problems);
        Ok(())
    }

    fn remove(&self, proxy: &ProxyAddr) -> Result<(), ProxySignInError> {
        self.passwords.remove(proxy)?;
        self.chain.forget_basic_headers();
        report_lost(&self.passwords, &self.problems);
        Ok(())
    }
}

#[cfg(test)]
mod tests;
