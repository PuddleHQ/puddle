// SPDX-License-Identifier: AGPL-3.0-or-later
//! The sign-in Puddle uses for a company proxy that asks for a user name and password (Basic),
//! kept per proxy in the operating system's credential store and nowhere else: never in a
//! settings file, never in the audit, never in a log.
//!
//! Each proxy has one entry, `proxy-<hash of host:port>`, holding the user name and the password.
//! One more entry, `proxy-list`, names the proxies that have one, because a credential store cannot
//! be listed and the proxy path must not wait for it: [`ProxyPasswords::load`] reads the list and
//! the entries once (off the async threads), and a sign-in is then answered from memory. A change
//! writes the list before the entry it adds and removes the entry before it takes its name off the
//! list, so a crash leaves a name without an entry, which loading skips, never a value no list names.
//! The addresses (never a user name or password) are also kept in a plain file next to the data, so
//! a store that comes back empty (a secrets reset) is noticed: [`ProxyPasswords::lost`] names the
//! proxies that had a sign-in and have none, and the host shows a problem until each is entered again.

use std::collections::BTreeMap;
use std::fmt::{self, Write as _};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use puddle_secrets::{Secret, SecretStore, StoreError, StoredId};
use puddle_upstream::{CredentialSource, Credentials, ProxyAddr};
use sha2::{Digest, Sha256};

/// What every id this module writes starts with. The clean-up of the credential store keeps
/// whatever starts with it: nothing in the database names these values.
pub(crate) const ID_PREFIX: &str = "proxy-";

/// Longest user name accepted, in bytes.
const MAX_USER: usize = 256;

/// Why a sign-in could not be stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ProxyPasswordError {
    /// The user name is empty, too long, or has a character a Basic user name cannot carry (a colon
    /// or a line break).
    #[error("the user name is empty, too long, or has a colon or a line break in it")]
    BadUser,
    /// The password is empty or has a line break in it.
    #[error("the password is empty or has a line break in it")]
    BadPassword,
    /// The operating system's credential store did not do what was asked (it may be locked).
    #[error("the operating system's credential store did not answer")]
    Store,
}

impl From<StoreError> for ProxyPasswordError {
    fn from(_: StoreError) -> Self {
        Self::Store
    }
}

/// The proxy sign-ins, in memory, backed by the credential store.
pub struct ProxyPasswords {
    vault: Arc<dyn SecretStore>,
    /// What every entry's id starts with; [`ID_PREFIX`] except in a test that must not touch the
    /// entries of a real Puddle on the same computer.
    prefix: String,
    /// Held across store calls, so two changes never interleave their list writes. Only changes
    /// and [`ProxyPasswords::load`] take it: a reader never waits for the credential store.
    changing: Mutex<()>,
    /// What is known now; held only to read or swap it, never across a store call, so the
    /// network-health page and the proxy's lookups stay quick while the store is slow.
    entries: Mutex<BTreeMap<ProxyAddr, Credentials>>,
    /// A plain file naming the proxies (addresses only) that have had a sign-in, outside the
    /// credential store: a store that comes back empty (a reset, a new vault) cannot also forget
    /// which proxies were signed in to.
    known_file: Option<PathBuf>,
    /// The proxies a list or the known file names but no entry answers for, as of the last load.
    lost: Mutex<Vec<ProxyAddr>>,
}

impl fmt::Debug for ProxyPasswords {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProxyPasswords").finish_non_exhaustive()
    }
}

impl ProxyPasswords {
    /// Sign-ins kept in `vault`; none are known until [`ProxyPasswords::load`] has run.
    #[must_use]
    pub fn new(vault: Arc<dyn SecretStore>) -> Arc<Self> {
        Self::with_prefix(vault, ID_PREFIX)
    }

    /// Like [`ProxyPasswords::new`] with entries named from `prefix` (it must start with `proxy-`
    /// for the credential store's clean-up to keep them): for a test against the real credential
    /// store that must not touch the entries of a Puddle installed on the same computer.
    #[must_use]
    pub fn with_prefix(vault: Arc<dyn SecretStore>, prefix: &str) -> Arc<Self> {
        Arc::new(Self {
            vault,
            prefix: prefix.to_owned(),
            changing: Mutex::default(),
            entries: Mutex::default(),
            known_file: None,
            lost: Mutex::default(),
        })
    }

    /// Like [`ProxyPasswords::new`], also naming the proxies in the plain file `known_file`, so
    /// [`ProxyPasswords::lost`] can say which sign-ins the credential store no longer holds.
    #[must_use]
    pub fn with_known_file(vault: Arc<dyn SecretStore>, known_file: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            vault,
            prefix: ID_PREFIX.to_owned(),
            changing: Mutex::default(),
            entries: Mutex::default(),
            known_file: Some(known_file),
            lost: Mutex::default(),
        })
    }

    /// Reads the list and every entry from the store; blocking, so run it off the async threads.
    /// Returns how many sign-ins are known. A listed proxy whose entry is missing or unreadable as a
    /// sign-in is skipped.
    ///
    /// # Errors
    /// [`ProxyPasswordError::Store`] when the store cannot be read at all; nothing changes then.
    pub fn load(&self) -> Result<usize, ProxyPasswordError> {
        let _changing = self.changing.lock().unwrap_or_else(PoisonError::into_inner);
        let mut loaded = BTreeMap::new();
        let mut lost = Vec::new();
        let mut names = self.listed()?;
        for proxy in self.known() {
            if !names.contains(&proxy) {
                names.push(proxy);
            }
        }
        for proxy in names {
            if let Some(secret) = self.vault.get(&self.entry_id(&proxy)?)?
                && let Some(credentials) = decode(&secret)
            {
                loaded.insert(proxy, credentials);
            } else {
                lost.push(proxy);
            }
        }
        let count = loaded.len();
        *self.entries() = loaded;
        *self.lost.lock().unwrap_or_else(PoisonError::into_inner) = lost;
        Ok(count)
    }

    /// The proxies that had a sign-in and have none now: named by the credential store's list or
    /// the known file, but no readable entry as of the last [`ProxyPasswords::load`] or change.
    /// Setting the proxy again, or removing it, takes it off.
    #[must_use]
    pub fn lost(&self) -> Vec<ProxyAddr> {
        self.lost
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The proxies the known file names; none when there is no file or it cannot be read.
    fn known(&self) -> Vec<ProxyAddr> {
        let Some(text) = self
            .known_file
            .as_ref()
            .and_then(|path| std::fs::read_to_string(path).ok())
        else {
            return Vec::new();
        };
        text.lines()
            .filter_map(|line| ProxyAddr::parse(line).ok())
            .collect()
    }

    /// Writes `names` to the known file; a file that cannot be written is logged and left, since
    /// the credential store's own list still names the proxy.
    fn write_known(&self, names: &[ProxyAddr]) {
        let Some(path) = &self.known_file else { return };
        let text = names
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        if let Err(err) = std::fs::write(path, text) {
            tracing::warn!(error = %err.kind(), "the proxy sign-in names could not be written");
        }
    }

    /// Stores `user` and `password` for `proxy`, replacing an earlier sign-in. The stored list is
    /// read again first, so a sign-in this run never loaded stays listed. Blocking.
    ///
    /// # Errors
    /// [`ProxyPasswordError::BadUser`] or [`ProxyPasswordError::BadPassword`] for a value Basic
    /// cannot carry, [`ProxyPasswordError::Store`] when the store failed; nothing is changed in
    /// memory then.
    pub fn set(
        &self,
        proxy: &ProxyAddr,
        user: &str,
        password: &str,
    ) -> Result<(), ProxyPasswordError> {
        if user.is_empty() || user.len() > MAX_USER || user.contains([':', '\n', '\r']) {
            return Err(ProxyPasswordError::BadUser);
        }
        if password.is_empty() || password.contains(['\n', '\r']) {
            return Err(ProxyPasswordError::BadPassword);
        }
        let _changing = self.changing.lock().unwrap_or_else(PoisonError::into_inner);
        let mut names = self.listed()?;
        if !names.contains(proxy) {
            names.push(proxy.clone());
        }
        self.write_list(&names)?;
        let value = Secret::new(format!("{user}\n{password}"));
        self.vault.set(&self.entry_id(proxy)?, &value)?;
        self.entries()
            .insert(proxy.clone(), Credentials::new(user, password));
        let mut known = self.known();
        if !known.contains(proxy) {
            known.push(proxy.clone());
            self.write_known(&known);
        }
        self.read_missing(&names);
        self.lost
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|p| p != proxy);
        Ok(())
    }

    /// Forgets the sign-in for `proxy`; one that is not stored is fine. Blocking.
    ///
    /// # Errors
    /// [`ProxyPasswordError::Store`] when the store failed.
    pub fn remove(&self, proxy: &ProxyAddr) -> Result<(), ProxyPasswordError> {
        let _changing = self.changing.lock().unwrap_or_else(PoisonError::into_inner);
        self.vault.delete(&self.entry_id(proxy)?)?;
        let mut names = self.listed()?;
        names.retain(|p| p != proxy);
        self.write_list(&names)?;
        self.entries().remove(proxy);
        let mut known = self.known();
        known.retain(|p| p != proxy);
        self.write_known(&known);
        self.read_missing(&names);
        self.lost
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|p| p != proxy);
        Ok(())
    }

    /// Reads the sign-ins of `listed` that are not in memory: a load that failed (a locked store
    /// at start) left them out, and a change that went through shows the store answers now. One
    /// that still cannot be read stays listed for a later load.
    fn read_missing(&self, listed: &[ProxyAddr]) {
        for proxy in listed {
            if self.entries().contains_key(proxy) {
                continue;
            }
            if let Ok(Some(secret)) = self.entry_id(proxy).and_then(|id| self.vault.get(&id))
                && let Some(credentials) = decode(&secret)
            {
                self.entries().insert(proxy.clone(), credentials);
            }
        }
    }

    /// The proxies that have a sign-in and the user name of each, never a password.
    #[must_use]
    pub fn proxies(&self) -> Vec<(ProxyAddr, String)> {
        self.entries()
            .iter()
            .map(|(proxy, credentials)| (proxy.clone(), credentials.user().to_owned()))
            .collect()
    }

    fn entries(&self) -> std::sync::MutexGuard<'_, BTreeMap<ProxyAddr, Credentials>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn listed(&self) -> Result<Vec<ProxyAddr>, StoreError> {
        let Some(list) = self.vault.get(&self.list_id()?)? else {
            return Ok(Vec::new());
        };
        Ok(list
            .expose()
            .lines()
            .filter_map(|line| ProxyAddr::parse(line).ok())
            .collect())
    }

    fn write_list(&self, names: &[ProxyAddr]) -> Result<(), StoreError> {
        if names.is_empty() {
            return self.vault.delete(&self.list_id()?);
        }
        let text = names
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        self.vault.set(&self.list_id()?, &Secret::new(text))
    }
}

impl CredentialSource for ProxyPasswords {
    fn credentials(&self, proxy: &ProxyAddr) -> Option<Credentials> {
        self.entries().get(proxy).cloned()
    }
}

/// The user name is the first line, the password the rest; one that does not read so is no sign-in.
fn decode(secret: &Secret) -> Option<Credentials> {
    let (user, password) = secret.expose().split_once('\n')?;
    (!user.is_empty() && !password.is_empty()).then(|| Credentials::new(user, password))
}

impl ProxyPasswords {
    /// The entry naming the proxies that have a sign-in. The ids are valid by construction; an
    /// error here would be the store refusing a name.
    fn list_id(&self) -> Result<StoredId, StoreError> {
        list_id(&self.prefix)
    }

    /// The prefix and 32 hex digits of the hash of `host:port`: a fixed-size id whatever the host
    /// name holds, the same for the same proxy on every run.
    fn entry_id(&self, proxy: &ProxyAddr) -> Result<StoredId, StoreError> {
        entry_id(&self.prefix, proxy)
    }
}

fn list_id(prefix: &str) -> Result<StoredId, StoreError> {
    StoredId::new(format!("{prefix}list")).map_err(|_| StoreError)
}

fn entry_id(prefix: &str, proxy: &ProxyAddr) -> Result<StoredId, StoreError> {
    let digest = Sha256::digest(proxy.to_string().as_bytes());
    let hex = digest.iter().take(16).fold(String::new(), |mut hex, byte| {
        // writing into a `String` cannot fail
        let _ = write!(hex, "{byte:02x}");
        hex
    });
    StoredId::new(format!("{prefix}{hex}")).map_err(|_| StoreError)
}

/// Every id the sign-ins occupy in `vault`: the list and the entry of each proxy it names. Read
/// from the store itself, because no row names them. A proxy whose entry is gone is still listed
/// here; the caller finds no value under it.
///
/// # Errors
/// [`StoreError`] when the list cannot be read.
pub(crate) fn stored_ids(vault: &dyn SecretStore) -> Result<Vec<StoredId>, StoreError> {
    let list = list_id(ID_PREFIX)?;
    let mut ids = vec![list.clone()];
    if let Some(text) = vault.get(&list)? {
        for proxy in text
            .expose()
            .lines()
            .filter_map(|l| ProxyAddr::parse(l).ok())
        {
            ids.push(entry_id(ID_PREFIX, &proxy)?);
        }
    }
    Ok(ids)
}

#[cfg(test)]
mod tests;
