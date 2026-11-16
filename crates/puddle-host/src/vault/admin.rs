// SPDX-License-Identifier: AGPL-3.0-or-later
//! `puddle secrets status`, `migrate` and `reset`: the operator's way to look at the store that
//! keeps secrets, move the values to the other store, or start a new file vault when the old one
//! cannot be opened.
//!
//! `migrate` and `reset` take the data-folder lock, so a running host makes them refuse and
//! names itself. `status` only reads and works beside a host. Nothing here returns an id or a
//! value: results are counts and the words of the problem.

use std::collections::HashSet;
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use puddle_fs::DataLock;
use puddle_secrets::{
    FileVault, IndexedStore, KeySourceTag, MigrateError, RecordError, Rule, SecretStore, StoreKind,
    StoreRecord, StoredId, ToolPaths, VaultKeys, clear_old_store, config_home, file_stamp,
    move_values, read_record, vault_path, write_record,
};
use puddle_store::{Limits, Store, SystemClock};

use super::choose::{OpenFailure, SecretSetup, open_file_vault, vault_failure};
use super::os_vault;
use crate::HostPaths;
use crate::proxy_passwords::stored_ids;

/// How long `status` waits for a store that may be waiting for the user to unlock it.
const STATUS_WAIT: Duration = Duration::from_secs(20);

/// Why a command could not finish. The texts carry no value, no secret id and no data-folder path.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum VaultAdminError {
    /// Another process holds the data folder.
    #[error("{0}")]
    Locked(#[from] puddle_fs::LockError),
    /// The rows that name secrets could not be read.
    #[error("the database could not be read to find which secrets it names: {0}")]
    Rows(#[from] puddle_store::StoreError),
    /// `secrets/store.json` could not be read or written.
    #[error("{0}")]
    Record(#[from] RecordError),
    /// The store holding the values could not be read or written.
    #[error("{0}")]
    Store(String),
    /// The file vault could not be opened or made.
    #[error("{title}. {fix}")]
    Vault {
        /// What is wrong.
        title: &'static str,
        /// What to do about it.
        fix: String,
    },
    /// The move stopped; nothing changed.
    #[error("{0}")]
    Move(#[from] MigrateError),
    /// `reset` was asked for a data folder that has not chosen a store.
    #[error("this data folder has not chosen a store yet, so there is no vault to reset")]
    Undecided,
    /// `reset` was asked for the operating system's store.
    #[error(
        "this data folder keeps its secrets in the operating system's credential store; `puddle secrets reset` only starts a new file vault"
    )]
    NotAFileVault,
    /// The vault the record names is not there.
    #[error("there is no vault file; the next start of Puddle makes a new one")]
    NoVault,
    /// `reset` would throw away a vault that works.
    #[error("the vault opens with its key, so there is nothing to reset")]
    VaultOpens,
    /// The file system refused a rename.
    #[error("the vault file could not be set aside: {0}")]
    Aside(String),
}

impl From<OpenFailure> for VaultAdminError {
    fn from(failure: OpenFailure) -> Self {
        Self::Vault {
            title: failure.title,
            fix: failure.fix,
        }
    }
}

/// What `puddle secrets status` found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultStatus {
    /// What `secrets/store.json` says; `None` before a start chose a store.
    pub record: Option<StoreRecord>,
    /// How many secrets the rows name (a token, an environment secret).
    pub named: usize,
    /// For the file vault: where its key comes from, as the vault's header says.
    pub key_source: Option<KeySourceTag>,
    /// Whether the store could be read.
    pub state: StoreState,
}

/// Whether the store could be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreState {
    /// No store is chosen, so none was asked.
    Undecided,
    /// The record names the file vault and there is no vault file yet.
    NoVault,
    /// The store answered; `found` of the named secrets are in it.
    Readable {
        /// How many of the named secrets the store holds.
        found: usize,
    },
    /// The store could not be read.
    Unreadable {
        /// What is wrong.
        title: String,
        /// What to do about it.
        fix: String,
    },
}

/// What `puddle secrets migrate` did, as counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrateOutcome {
    /// The data folder had not chosen and `os` is what every earlier install used: only the
    /// record was written.
    Recorded,
    /// The folder already keeps its secrets there and no earlier move is unfinished.
    AlreadyThere(StoreKind),
    /// Values moved to the other store, which is now in charge.
    Moved {
        /// Values in the new store.
        copied: usize,
        /// Values the old store would not give up; a rerun removes them.
        left: usize,
    },
    /// A move that stopped after the switch is finished: leftovers removed from the old store.
    Finished {
        /// Values removed.
        removed: usize,
        /// Values the old store would not give up.
        left: usize,
    },
}

/// What `puddle secrets reset` did or would do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetOutcome {
    /// Without `--yes`: what would happen.
    Plan {
        /// How many key files (the sealed key and the key file) would be set aside.
        key_files: usize,
    },
    /// With `--yes`: the old vault and keys are set aside and a new, empty vault exists.
    Done {
        /// How many key files were set aside.
        key_files: usize,
    },
}

/// Reads one future to its end on a runtime of its own: these commands run before any host.
fn block_on<T>(future: impl Future<Output = T>) -> Result<T, VaultAdminError> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map(|runtime| runtime.block_on(future))
        .map_err(|err| VaultAdminError::Store(format!("cannot start the async runtime: {err}")))
}

fn key_home(setup: &SecretSetup) -> Option<PathBuf> {
    setup.key_home.clone().or_else(config_home)
}

fn tools(setup: &SecretSetup) -> ToolPaths {
    setup.tools.clone().unwrap_or_else(ToolPaths::resolve)
}

/// The ids the rows name.
fn named_ids(paths: &HostPaths) -> Result<HashSet<StoredId>, VaultAdminError> {
    let database = paths.store();
    if !database.is_file() {
        return Ok(HashSet::new());
    }
    let store = Store::open(&database, Arc::new(SystemClock), Limits::default())?;
    Ok(store.secret_ids_in_use()?)
}

fn os_store(setup: &SecretSetup) -> Arc<dyn SecretStore> {
    setup.os_store.clone().unwrap_or_else(os_vault)
}

/// The file vault, with its key. `make` lets a missing vault (and its key) be made; without it a
/// missing vault is `Ok(None)`.
fn file_store(
    paths: &HostPaths,
    setup: &SecretSetup,
    make: bool,
) -> Result<Option<Arc<dyn SecretStore>>, VaultAdminError> {
    let vault = vault_path(paths.data());
    if !make && !vault.exists() {
        return Ok(None);
    }
    let home = key_home(setup);
    let opened = block_on(open_file_vault(home.as_deref(), &vault, tools(setup)))?;
    let store: Arc<dyn SecretStore> = Arc::new(IndexedStore::new(opened?));
    Ok(Some(store))
}

/// Every id `old` holds that Puddle knows to ask for: the store's own list, the ids the rows
/// name and the proxy sign-ins, which no row names.
fn ids_in(
    old: &dyn SecretStore,
    named: &HashSet<StoredId>,
) -> Result<Vec<StoredId>, VaultAdminError> {
    let mut ids = old.listed_ids().map_err(|_| MigrateError::Source)?;
    ids.extend(named.iter().cloned());
    ids.extend(stored_ids(old).map_err(|_| MigrateError::Source)?);
    Ok(ids)
}

const fn other(kind: StoreKind) -> StoreKind {
    match kind {
        StoreKind::Os => StoreKind::File,
        StoreKind::File => StoreKind::Os,
    }
}

/// Moves every secret to the store `to` and records it as the one in charge, or finishes such a
/// move that stopped after the record was written.
///
/// # Errors
/// [`VaultAdminError`] when another process holds the data folder, a store cannot be read or
/// written, or a value does not arrive intact. The old store keeps every value then.
pub fn vault_migrate(
    paths: &HostPaths,
    setup: &SecretSetup,
    to: StoreKind,
) -> Result<MigrateOutcome, VaultAdminError> {
    let _lock = DataLock::acquire(paths.data())?;
    let data = paths.data();
    let record = read_record(data)?;
    // A folder with no record is one every earlier install made: its values are in the
    // operating system's store.
    let current = record.as_ref().map_or(StoreKind::Os, |r| r.store);
    let named = named_ids(paths)?;
    if current == to {
        return match record {
            None => {
                write_record(data, to, Rule::Migrated)?;
                Ok(MigrateOutcome::Recorded)
            }
            Some(record) if record.why == Rule::Migrated.why() => finish(paths, setup, to, &named),
            Some(_) => Ok(MigrateOutcome::AlreadyThere(to)),
        };
    }
    let old = match current {
        StoreKind::Os => Some(os_store(setup)),
        StoreKind::File => file_store(paths, setup, false)?,
    }
    .ok_or(VaultAdminError::NoVault)?;
    let new = match to {
        StoreKind::Os => os_store(setup),
        StoreKind::File => file_store(paths, setup, true)?.ok_or(VaultAdminError::NoVault)?,
    };
    let ids = ids_in(old.as_ref(), &named)?;
    let moved = move_values(old.as_ref(), new.as_ref(), &ids, &mut || {
        write_record(data, to, Rule::Migrated)
    })?;
    Ok(MigrateOutcome::Moved {
        copied: moved.copied,
        left: moved.left,
    })
}

/// A move that stopped after the switch: the store in charge keeps what it has, the other one
/// is emptied.
fn finish(
    paths: &HostPaths,
    setup: &SecretSetup,
    in_charge: StoreKind,
    named: &HashSet<StoredId>,
) -> Result<MigrateOutcome, VaultAdminError> {
    let old = match other(in_charge) {
        StoreKind::Os => Some(os_store(setup)),
        StoreKind::File => file_store(paths, setup, false)?,
    };
    let Some(old) = old else {
        return Ok(MigrateOutcome::Finished {
            removed: 0,
            left: 0,
        });
    };
    let ids = ids_in(old.as_ref(), named)?;
    let cleared = clear_old_store(old.as_ref(), &ids);
    Ok(MigrateOutcome::Finished {
        removed: cleared.removed,
        left: cleared.left,
    })
}

/// Which store this data folder keeps its secrets in, and whether it can be read. Reads only,
/// takes no lock, and gives a locked keyring [`STATUS_WAIT`] to answer.
///
/// # Errors
/// [`VaultAdminError`] when the record or the rows cannot be read.
pub fn vault_status(
    paths: &HostPaths,
    setup: &SecretSetup,
) -> Result<VaultStatus, VaultAdminError> {
    status_within(paths, setup, STATUS_WAIT)
}

fn status_within(
    paths: &HostPaths,
    setup: &SecretSetup,
    wait: Duration,
) -> Result<VaultStatus, VaultAdminError> {
    let record = read_record(paths.data())?;
    let named = named_ids(paths)?;
    let mut status = VaultStatus {
        named: named.len(),
        key_source: None,
        state: StoreState::Undecided,
        record: record.clone(),
    };
    let Some(record) = record else {
        return Ok(status);
    };
    let store = match record.store {
        StoreKind::Os => os_store(setup),
        StoreKind::File => {
            let vault = vault_path(paths.data());
            match FileVault::header_source(&vault) {
                Ok(None) => {
                    status.state = StoreState::NoVault;
                    return Ok(status);
                }
                Ok(tag) => status.key_source = tag,
                Err(err) => {
                    status.state = unreadable(vault_failure(err));
                    return Ok(status);
                }
            }
            let home = key_home(setup);
            match block_on(open_file_vault(home.as_deref(), &vault, tools(setup)))? {
                Ok(opened) => Arc::new(IndexedStore::new(opened)),
                Err(failure) => {
                    status.state = unreadable(failure);
                    return Ok(status);
                }
            }
        }
    };
    status.state = read_within(store, named, wait, record.store);
    Ok(status)
}

fn unreadable(failure: OpenFailure) -> StoreState {
    StoreState::Unreadable {
        title: failure.title.to_owned(),
        fix: failure.fix,
    }
}

/// Counts how many of `named` the store holds, off this thread so a keyring that waits for an
/// unlock cannot hold the command for longer than `wait`.
fn read_within(
    store: Arc<dyn SecretStore>,
    named: HashSet<StoredId>,
    wait: Duration,
    kind: StoreKind,
) -> StoreState {
    let (send, receive) = mpsc::channel();
    std::thread::spawn(move || {
        let counted = store.listed_ids().and_then(|_| {
            let mut found = 0;
            for id in &named {
                if store.get(id)?.is_some() {
                    found += 1;
                }
            }
            Ok(found)
        });
        // The command may have given up waiting; nobody listens then.
        let _ = send.send(counted);
    });
    match receive.recv_timeout(wait) {
        Ok(Ok(found)) => StoreState::Readable { found },
        Ok(Err(_)) => StoreState::Unreadable {
            title: format!("{} could not be read", kind.describe()),
            fix: "If it is locked, unlock it (sign in to the account that runs Puddle, or unlock the keyring), then run this command again.".to_owned(),
        },
        Err(_) => StoreState::Unreadable {
            title: format!(
                "{} did not answer within {} seconds",
                kind.describe(),
                wait.as_secs()
            ),
            fix: "It may be waiting for you to unlock it; unlock it and run this command again."
                .to_owned(),
        },
    }
}

/// Starts a new file vault when the recorded one does not open: the old vault and keys are set
/// aside, never deleted. Without `yes` nothing changes and the result says what would.
///
/// # Errors
/// [`VaultAdminError`]: the folder is held by a running host, keeps its secrets in the
/// operating system's store, has not chosen, has no vault, or has a vault that opens.
pub fn vault_reset(
    paths: &HostPaths,
    setup: &SecretSetup,
    yes: bool,
) -> Result<ResetOutcome, VaultAdminError> {
    let _lock = DataLock::acquire(paths.data())?;
    let data = paths.data();
    match read_record(data)?.map(|record| record.store) {
        None => return Err(VaultAdminError::Undecided),
        Some(StoreKind::Os) => return Err(VaultAdminError::NotAFileVault),
        Some(StoreKind::File) => {}
    }
    let vault = vault_path(data);
    if !vault.exists() {
        return Err(VaultAdminError::NoVault);
    }
    let home = key_home(setup);
    let Err(failure) = block_on(open_file_vault(home.as_deref(), &vault, tools(setup)))? else {
        return Err(VaultAdminError::VaultOpens);
    };
    // Without a place for keys there is no vault to start either: the failure says why.
    let keys = home
        .as_deref()
        .map(|home| VaultKeys::new(home, &vault, tools(setup)))
        .ok_or(VaultAdminError::from(failure))?;
    if !yes {
        return Ok(ResetOutcome::Plan {
            key_files: keys.keys_to_set_aside(),
        });
    }
    let stamp = file_stamp();
    let mut aside = vault.clone().into_os_string();
    aside.push(format!(".unreadable-{stamp}"));
    std::fs::rename(&vault, PathBuf::from(aside))
        .map_err(|err| VaultAdminError::Aside(err.kind().to_string()))?;
    let key_files = keys
        .set_keys_aside(&stamp)
        .map_err(|err| VaultAdminError::Aside(err.to_string()))?;
    // No vault and no key now, so this makes both.
    block_on(open_file_vault(home.as_deref(), &vault, tools(setup)))??;
    Ok(ResetOutcome::Done { key_files })
}

#[cfg(test)]
mod tests;
