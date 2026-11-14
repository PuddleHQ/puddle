// SPDX-License-Identifier: AGPL-3.0-or-later
//! Which store this start keeps secrets in, and the store built for it.
//!
//! The rules are `puddle_secrets::decide`; this module gathers what they need, builds the store
//! (the operating system's, or the file vault with its key), writes the decision to
//! `secrets/store.json` and turns a store that cannot be opened into a problem. A host whose
//! store cannot be opened still starts: it gets a store that refuses every call, because a
//! missing secret stops tokens, not sandboxes.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use puddle_secrets::{
    ChoiceError, FileVault, IndexedStore, Inputs, KeyError, LoginSession, Recording, Rule,
    SecretStore, StoreKind, StoreOption, ToolPaths, Unseal, VaultError, VaultKeys, config_home,
    decide, login_session, read_record, vault_path, write_record,
};
use puddle_store::Store;
use puddle_types::{Problem, Problems};

use super::{STORE_PROBLEM, os_vault};
use crate::error::HostError;
use crate::paths::HostPaths;

/// How a start picks and opens the store. The defaults are the shipped host's; the fields exist
/// so a test never reads the real machine.
#[derive(Clone, Default)]
#[non_exhaustive]
pub struct SecretSetup {
    /// The `secret_store` option.
    pub option: StoreOption,
    /// The headless flag of `puddle serve`: on Linux, a first start picks the file vault.
    pub headless: bool,
    /// The login session to report instead of asking `loginctl` (tests).
    pub session: Option<LoginSession>,
    /// The user's config folder, where the vault's key lives; the real one when `None` (tests).
    pub key_home: Option<PathBuf>,
    /// Where `systemd-creds` and `loginctl` are; looked up on `PATH` when `None` (tests).
    pub tools: Option<ToolPaths>,
    /// A store to use as the operating system's (tests); the real one when `None`.
    pub os_store: Option<Arc<dyn SecretStore>>,
}

/// What to write to the store at start, to see that it works.
#[derive(Clone)]
pub(super) enum Probe {
    /// A store the host was handed, or an operating-system store a test supplied.
    Store(Arc<dyn SecretStore>),
    /// The platform's own credential store.
    Os,
    /// The file vault.
    File(Arc<dyn SecretStore>),
    /// Nothing: the store could not be opened and a problem says why.
    Skip,
}

/// A decision still to be written once the operating system's store has taken a write.
#[derive(Clone)]
pub(super) struct WriteWhenWorking {
    pub(super) data: PathBuf,
    pub(super) rule: Rule,
}

/// The store a start uses, with what its start-up check needs.
pub(crate) struct Chosen {
    pub(crate) vault: Arc<dyn SecretStore>,
    pub(super) probe: Probe,
    pub(super) record_when_working: Option<WriteWhenWorking>,
    /// Set when the start is one that deserves the headless warning: Linux, the operating
    /// system's store, no flag, no explicit option. Holds what `loginctl` said.
    pub(super) warn_without_flag: Option<LoginSession>,
    /// `false` when the store could not be opened (the sweep would only add a second problem).
    pub(crate) usable: bool,
}

/// Picks and opens the store.
///
/// # Errors
/// [`HostError`] for what a start must not go on with: an option that disagrees with the store the
/// folder recorded, a record that cannot be read or written, rows that cannot be read.
pub(crate) async fn open(
    fake: Option<&Arc<dyn SecretStore>>,
    setup: &SecretSetup,
    paths: &HostPaths,
    store: &Store,
    problems: &Problems,
) -> Result<Chosen, HostError> {
    if let Some(fake) = fake {
        return Ok(Chosen {
            vault: fake.clone(),
            probe: Probe::Store(fake.clone()),
            record_when_working: None,
            warn_without_flag: None,
            usable: true,
        });
    }
    let data = paths.data();
    let recorded = read_record(data)?.map(|record| record.store);
    let names_secret = if recorded.is_none() && setup.option == StoreOption::Auto {
        !store.secret_ids_in_use()?.is_empty()
    } else {
        false
    };
    let decision = decide(Inputs {
        option: setup.option,
        recorded,
        names_secret,
        linux: cfg!(target_os = "linux"),
        headless: setup.headless,
    })?;
    let tools = setup.tools.clone().unwrap_or_else(ToolPaths::resolve);
    tracing::info!(
        store = decision.store.as_str(),
        why = decision.rule.why(),
        "secret store chosen"
    );
    match decision.store {
        StoreKind::Os => {
            if decision.recording == Recording::Now {
                write_record(data, StoreKind::Os, decision.rule)?;
            }
            let warn =
                cfg!(target_os = "linux") && !setup.headless && setup.option == StoreOption::Auto;
            let warn_without_flag = if warn {
                Some(match setup.session {
                    Some(session) => session,
                    None => login_session(&tools).await,
                })
            } else {
                None
            };
            let (vault, probe) = match &setup.os_store {
                Some(store) => (store.clone(), Probe::Store(store.clone())),
                None => (os_vault(), Probe::Os),
            };
            Ok(Chosen {
                vault,
                probe,
                record_when_working: (decision.recording == Recording::WhenWorking).then(|| {
                    WriteWhenWorking {
                        data: data.to_path_buf(),
                        rule: decision.rule,
                    }
                }),
                warn_without_flag,
                usable: true,
            })
        }
        StoreKind::File => {
            let key_home = setup.key_home.clone().or_else(config_home);
            match open_file_vault(key_home.as_deref(), &vault_path(data), tools).await {
                Ok(vault) => {
                    if decision.recording == Recording::Now {
                        write_record(data, StoreKind::File, decision.rule)?;
                    }
                    let vault: Arc<dyn SecretStore> = Arc::new(IndexedStore::new(vault));
                    Ok(Chosen {
                        vault: vault.clone(),
                        probe: Probe::File(vault),
                        record_when_working: None,
                        warn_without_flag: None,
                        usable: true,
                    })
                }
                Err(failure) => {
                    tracing::warn!(cause = %failure.cause, "the file vault could not be opened");
                    problems.raise(Problem::new(STORE_PROBLEM, failure.title, failure.fix));
                    Ok(Chosen {
                        vault: Arc::new(Refusing),
                        probe: Probe::Skip,
                        record_when_working: None,
                        warn_without_flag: None,
                        usable: false,
                    })
                }
            }
        }
    }
}

/// A store that refuses every call, for a start whose real store could not be opened.
struct Refusing;

impl SecretStore for Refusing {
    fn get(
        &self,
        _: &puddle_secrets::StoredId,
    ) -> Result<Option<puddle_secrets::Secret>, puddle_secrets::StoreError> {
        Err(puddle_secrets::StoreError)
    }
    fn set(
        &self,
        _: &puddle_secrets::StoredId,
        _: &puddle_secrets::Secret,
    ) -> Result<(), puddle_secrets::StoreError> {
        Err(puddle_secrets::StoreError)
    }
    fn delete(&self, _: &puddle_secrets::StoredId) -> Result<(), puddle_secrets::StoreError> {
        Err(puddle_secrets::StoreError)
    }
}

/// Why the file vault could not be opened: the problem's words and the log's.
pub(super) struct OpenFailure {
    pub(super) title: &'static str,
    pub(super) fix: String,
    /// What went wrong, in a line a log may carry (no path content, no key).
    pub(super) cause: String,
}

/// Finds the key, then opens the vault, or makes both on the first start.
async fn open_file_vault(
    key_home: Option<&Path>,
    vault: &Path,
    tools: ToolPaths,
) -> Result<FileVault, OpenFailure> {
    let Some(key_home) = key_home else {
        return Err(key_failure(&KeyError::NoKeySource));
    };
    let found = VaultKeys::new(key_home, vault, tools)
        .find_or_make()
        .await
        .map_err(|err| key_failure(&err))?;
    let opened = if vault.exists() {
        FileVault::open(vault, found.key)
    } else {
        FileVault::create(vault, found.key, found.tag)
    };
    opened.map_err(vault_failure)
}

const KEY_FOLDER: &str =
    "the `puddle` folder of your config folder (`$XDG_CONFIG_HOME`, or `~/.config`)";

fn key_failure(err: &KeyError) -> OpenFailure {
    let (title, fix) = match err {
        KeyError::NoKeySource => (
            "Puddle cannot open its file vault: this computer has no place for its key",
            "The file vault's key is kept by systemd or in a key file in your config folder, and this computer offers neither. Set `secret_store` to `os` (or `auto`) to use the operating system's credential store, then restart Puddle.".to_owned(),
        ),
        KeyError::WrongSource { header, found } => (
            "Puddle cannot open its file vault: its key is in another place than before",
            format!(
                "The vault was sealed with a {} key, and Puddle found a {} key instead. Puddle never makes a new key over an existing vault. Put the original key back (the key is kept in {KEY_FOLDER}, or passed by the service as the credential `puddle-vault-key`) and restart Puddle.",
                header.as_str(),
                found.as_str()
            ),
        ),
        KeyError::KeyMissing(tag) => (
            "Puddle cannot open its file vault: its key is missing",
            format!(
                "The vault was sealed with a {} key and that key is not here. Restore it from a copy of your config folder ({KEY_FOLDER}) and restart Puddle. If it is gone, the secrets in the vault cannot be read: stop Puddle, move the `secrets/vault` file in the data folder aside, restart, and enter the secrets again.",
                tag.as_str()
            ),
        ),
        KeyError::UnitCredentialInvalid => (
            "Puddle cannot open its file vault: the service's key is not a vault key",
            "The credential `puddle-vault-key` that the service passes is not a 32-byte key. Seal the key that opens this vault again, or remove the credential from the unit, then restart Puddle.".to_owned(),
        ),
        KeyError::KeyFileUnsafe(why) => (
            "Puddle cannot open its file vault: its key file can be read by others",
            format!("The vault's key file is not safe to use ({why}). Run `chmod 600` on `vault.key` in {KEY_FOLDER} and restart Puddle."),
        ),
        KeyError::KeyFileInvalid => (
            "Puddle cannot open its file vault: its key file is damaged",
            format!("`vault.key` in {KEY_FOLDER} is not a 32-byte key. Restore it from a copy and restart Puddle."),
        ),
        KeyError::Sealed(why) => (
            "Puddle cannot open its file vault: its sealed key does not open",
            format!(
                "{} Restore the machine or the setting that sealed it and restart Puddle. If that is not possible the secrets in the vault cannot be read: stop Puddle, move the `secrets/vault` file in the data folder and `vault-key.cred` in {KEY_FOLDER} aside, restart, and enter the secrets again.",
                unseal_cause(*why)
            ),
        ),
        KeyError::Vault(why) => return vault_failure(*why),
        KeyError::Io => (
            "Puddle cannot open its file vault: its key cannot be read or written",
            format!("Check the permissions and the free space of {KEY_FOLDER}, then restart Puddle."),
        ),
        KeyError::Random => (
            "Puddle cannot make a key for its file vault: the system's random source failed",
            "Restart Puddle; if it fails again the computer's random source needs attention.".to_owned(),
        ),
    };
    OpenFailure {
        title,
        fix,
        cause: err.to_string(),
    }
}

fn unseal_cause(why: Unseal) -> &'static str {
    match why {
        Unseal::ToolMissing => "`systemd-creds` is not installed, and it opens the sealed key.",
        Unseal::Unsupported => {
            "This systemd is older than 256, which cannot open a key sealed for a user."
        }
        Unseal::Rejected => {
            "systemd refused to open it: the machine id, the host key or the Secure Boot setting is not what it was when the key was sealed."
        }
        Unseal::NameMismatch => "The key was sealed under another name.",
        Unseal::Failed | Unseal::TimedOut => {
            "`systemd-creds` did not answer, so its service may not be running."
        }
        Unseal::NotAKey => "`systemd-creds` answered something that is not a vault key.",
    }
}

fn vault_failure(err: VaultError) -> OpenFailure {
    let fix = match err {
        VaultError::WrongKey => {
            "The key Puddle found does not open the vault, so the vault was made with another key. Put the original key back and restart Puddle."
        }
        VaultError::UnknownVersion(_) => {
            "The vault was written by a newer version of Puddle. Start that version, or update this one."
        }
        VaultError::Io | VaultError::Missing | VaultError::Exists | VaultError::Random => {
            "Check the permissions and the free space of the `secrets` folder in the data folder and restart Puddle."
        }
        VaultError::NotAVault | VaultError::Corrupt => {
            "The vault file is damaged. Restore `secrets/vault` in the data folder from a copy; if there is none, move it aside, restart and enter the secrets again."
        }
    };
    OpenFailure {
        title: "Puddle cannot open its file vault: the vault file cannot be read",
        fix: fix.to_owned(),
        cause: err.to_string(),
    }
}

/// The message of a start refused for its choice of store.
impl From<ChoiceError> for HostError {
    fn from(err: ChoiceError) -> Self {
        Self::SecretStore(err.to_string())
    }
}

impl From<puddle_secrets::RecordError> for HostError {
    fn from(err: puddle_secrets::RecordError) -> Self {
        Self::SecretStore(err.to_string())
    }
}

#[cfg(test)]
mod tests;
