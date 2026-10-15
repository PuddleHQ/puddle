// SPDX-License-Identifier: GPL-3.0-or-later
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

use std::collections::BTreeMap;
use std::fmt::{self, Write as _};
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
    /// Held across store calls too, so two changes never interleave their list writes.
    entries: Mutex<BTreeMap<ProxyAddr, Credentials>>,
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

    fn with_prefix(vault: Arc<dyn SecretStore>, prefix: &str) -> Arc<Self> {
        Arc::new(Self {
            vault,
            prefix: prefix.to_owned(),
            entries: Mutex::default(),
        })
    }

    /// Reads the list and every entry from the store; blocking, so run it off the async threads.
    /// Returns how many sign-ins are known. A listed proxy whose entry is missing or unreadable as a
    /// sign-in is skipped.
    ///
    /// # Errors
    /// [`ProxyPasswordError::Store`] when the store cannot be read at all; nothing changes then.
    pub fn load(&self) -> Result<usize, ProxyPasswordError> {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        let mut loaded = BTreeMap::new();
        for proxy in self.listed()? {
            if let Some(secret) = self.vault.get(&self.entry_id(&proxy)?)?
                && let Some(credentials) = decode(&secret)
            {
                loaded.insert(proxy, credentials);
            }
        }
        let count = loaded.len();
        *entries = loaded;
        Ok(count)
    }

    /// Stores `user` and `password` for `proxy`, replacing an earlier sign-in. Blocking.
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
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        let mut names: Vec<ProxyAddr> = entries.keys().cloned().collect();
        if !names.contains(proxy) {
            names.push(proxy.clone());
        }
        self.write_list(&names)?;
        let value = Secret::new(format!("{user}\n{password}"));
        self.vault.set(&self.entry_id(proxy)?, &value)?;
        entries.insert(proxy.clone(), Credentials::new(user, password));
        Ok(())
    }

    /// Forgets the sign-in for `proxy`; one that is not stored is fine. Blocking.
    ///
    /// # Errors
    /// [`ProxyPasswordError::Store`] when the store failed.
    pub fn remove(&self, proxy: &ProxyAddr) -> Result<(), ProxyPasswordError> {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        self.vault.delete(&self.entry_id(proxy)?)?;
        let names: Vec<ProxyAddr> = entries.keys().filter(|p| *p != proxy).cloned().collect();
        self.write_list(&names)?;
        entries.remove(proxy);
        Ok(())
    }

    /// The proxies that have a sign-in and the user name of each, never a password.
    #[must_use]
    pub fn proxies(&self) -> Vec<(ProxyAddr, String)> {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(proxy, credentials)| (proxy.clone(), credentials.user().to_owned()))
            .collect()
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
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(proxy)
            .cloned()
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
        StoredId::new(format!("{}list", self.prefix)).map_err(|_| StoreError)
    }

    /// The prefix and 32 hex digits of the hash of `host:port`: a fixed-size id whatever the host
    /// name holds, the same for the same proxy on every run.
    fn entry_id(&self, proxy: &ProxyAddr) -> Result<StoredId, StoreError> {
        let digest = Sha256::digest(proxy.to_string().as_bytes());
        let hex = digest.iter().take(16).fold(String::new(), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        });
        StoredId::new(format!("{}{hex}", self.prefix)).map_err(|_| StoreError)
    }
}

#[cfg(test)]
mod tests {
    use puddle_secrets::MemoryStore;

    use super::*;

    const PASSWORD: &str = "hunter2-canary";

    fn proxy() -> ProxyAddr {
        ProxyAddr::new("Proxy.Corp", 8080)
    }

    fn vault() -> Arc<MemoryStore> {
        Arc::new(MemoryStore::new())
    }

    #[test]
    fn a_sign_in_survives_a_restart_and_answers_from_memory() {
        let vault = vault();
        let first = ProxyPasswords::new(vault.clone());
        first.set(&proxy(), "corp\\alice", PASSWORD).unwrap();
        assert_eq!(first.credentials(&proxy()).unwrap().user(), "corp\\alice");

        let second = ProxyPasswords::new(vault);
        assert!(
            second.credentials(&proxy()).is_none(),
            "nothing is known before load"
        );
        assert_eq!(second.load().unwrap(), 1);
        assert_eq!(second.proxies(), [(proxy(), "corp\\alice".to_owned())]);
        assert!(
            second
                .credentials(&ProxyAddr::new("other.corp", 8080))
                .is_none()
        );
    }

    #[test]
    fn nothing_but_the_credential_store_holds_the_password() {
        let vault = vault();
        let passwords = ProxyPasswords::new(vault.clone());
        passwords.set(&proxy(), "alice", PASSWORD).unwrap();
        for text in [
            format!("{passwords:?}"),
            format!("{:?}", passwords.proxies()),
        ] {
            assert!(!text.contains(PASSWORD), "{text}");
        }
        // Two entries, both under the prefix the clean-up keeps; the id does not spell the host.
        let ids = vault.ids();
        assert_eq!(ids.len(), 2, "{ids:?}");
        assert!(ids.iter().all(|id| id.starts_with(ID_PREFIX)), "{ids:?}");
        assert!(ids.iter().all(|id| !id.contains("corp")), "{ids:?}");
    }

    #[test]
    fn removing_a_sign_in_leaves_nothing_in_the_store() {
        let vault = vault();
        let passwords = ProxyPasswords::new(vault.clone());
        passwords.set(&proxy(), "alice", PASSWORD).unwrap();
        passwords.remove(&proxy()).unwrap();
        passwords.remove(&proxy()).unwrap();
        assert!(passwords.credentials(&proxy()).is_none());
        assert_eq!(passwords.proxies(), []);
        assert_eq!(vault.ids(), Vec::<String>::new());
    }

    #[test]
    fn setting_again_replaces_and_each_proxy_keeps_its_own() {
        let passwords = ProxyPasswords::new(vault());
        let other = ProxyAddr::new("other.corp", 3128);
        passwords.set(&proxy(), "alice", "one").unwrap();
        passwords.set(&other, "bob", "two").unwrap();
        passwords.set(&proxy(), "alice", "three").unwrap();
        assert_eq!(
            passwords.proxies(),
            [(other, "bob".to_owned()), (proxy(), "alice".to_owned())]
        );
        let again = ProxyPasswords::new(passwords.vault.clone());
        assert_eq!(again.load().unwrap(), 2);
    }

    #[test]
    fn a_user_or_password_basic_cannot_carry_is_refused_and_nothing_is_stored() {
        let vault = vault();
        let passwords = ProxyPasswords::new(vault.clone());
        for user in ["", "a:b", "a\nb", "a\rb", &"u".repeat(257)] {
            assert_eq!(
                passwords.set(&proxy(), user, "p"),
                Err(ProxyPasswordError::BadUser),
                "{user:?}"
            );
        }
        for password in ["", "a\nb", "a\rb"] {
            assert_eq!(
                passwords.set(&proxy(), "alice", password),
                Err(ProxyPasswordError::BadPassword),
                "{password:?}"
            );
        }
        assert_eq!(vault.ids(), Vec::<String>::new());
        // A password may hold a colon: Basic splits at the first one only.
        passwords.set(&proxy(), "alice", "pa:ss").unwrap();
    }

    #[test]
    fn a_store_that_fails_changes_nothing_in_memory() {
        let vault = vault();
        let passwords = ProxyPasswords::new(vault.clone());
        passwords.set(&proxy(), "alice", "one").unwrap();
        vault.break_it();
        assert_eq!(
            passwords.set(&proxy(), "alice", "two"),
            Err(ProxyPasswordError::Store)
        );
        assert_eq!(passwords.remove(&proxy()), Err(ProxyPasswordError::Store));
        assert_eq!(passwords.load(), Err(ProxyPasswordError::Store));
        // The old sign-in still answers, and still after the store is back.
        assert!(passwords.credentials(&proxy()).is_some());
        vault.heal();
        assert_eq!(passwords.load().unwrap(), 1);
    }

    #[test]
    fn a_listed_proxy_without_a_readable_entry_is_skipped_on_load() {
        let vault = vault();
        let passwords = ProxyPasswords::new(vault.clone());
        let other = ProxyAddr::new("other.corp", 3128);
        passwords.set(&proxy(), "alice", "one").unwrap();
        passwords.set(&other, "bob", "two").unwrap();
        // A crash between the list and the entry, and an entry that is not a sign-in.
        vault
            .delete(&passwords.entry_id(&proxy()).unwrap())
            .unwrap();
        vault
            .set(
                &passwords.entry_id(&other).unwrap(),
                &Secret::new("no newline".into()),
            )
            .unwrap();
        let again = ProxyPasswords::new(vault);
        assert_eq!(again.load().unwrap(), 0);
        assert_eq!(again.proxies(), []);
    }

    #[test]
    fn a_list_line_that_is_not_a_proxy_is_ignored() {
        let vault = vault();
        let passwords = ProxyPasswords::new(vault.clone());
        vault
            .set(
                &passwords.list_id().unwrap(),
                &Secret::new("\n://\nproxy.corp:8080".into()),
            )
            .unwrap();
        assert_eq!(passwords.load().unwrap(), 0);
    }

    /// The real operating system store, through entries no real Puddle uses (the prefix has this
    /// process's id in it). It runs where a store exists (the Windows job, a desktop session) and
    /// says so where it does not, unless `PUDDLE_REQUIRE_OS_CREDENTIAL_STORE` is set (the Windows
    /// job sets it): then a missing store fails.
    #[test]
    fn the_os_store_keeps_a_test_owned_proxy_sign_in() {
        if !puddle_secrets::keyring_available() {
            assert!(
                std::env::var_os("PUDDLE_REQUIRE_OS_CREDENTIAL_STORE").is_none(),
                "PUDDLE_REQUIRE_OS_CREDENTIAL_STORE is set but this computer has no credential store that works"
            );
            eprintln!("no OS credential store here; skipped");
            return;
        }
        let prefix = format!("proxy-selftest-{}-", std::process::id());
        let os = || -> Arc<dyn SecretStore> {
            Arc::new(puddle_secrets::ChunkedStore::new(
                puddle_secrets::KeyringStore::new(),
            ))
        };
        let proxy = ProxyAddr::new("selftest.invalid", 3128);
        let first = ProxyPasswords::with_prefix(os(), &prefix);
        first.set(&proxy, "alice", PASSWORD).unwrap();
        let second = ProxyPasswords::with_prefix(os(), &prefix);
        assert_eq!(second.load().unwrap(), 1);
        assert_eq!(second.credentials(&proxy).unwrap().user(), "alice");
        second.remove(&proxy).unwrap();
        let third = ProxyPasswords::with_prefix(os(), &prefix);
        assert_eq!(third.load().unwrap(), 0);
    }
}
