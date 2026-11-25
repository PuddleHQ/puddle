// SPDX-License-Identifier: AGPL-3.0-or-later
//! Where Puddle gets a secret on the host.
//!
//! Three sources behind one interface, [`Fetch`]:
//!
//! - **`gh`**: the token of a named, signed-in GitHub CLI account.
//! - **Git credential**: one HTTPS host and the account (or, without one, the URL path) that picks
//!   the entry, asked of Git Credential Manager (or whatever credential helper Git is set up with)
//!   through `git credential fill`.
//! - **Stored**: a token the user pasted, kept in the OS credential store under Puddle's own name.
//!
//! The list is closed ([`SourceSpec`]): no variant carries a command line, and every name that
//! reaches a child process is validated first. No source ever opens a sign-in window: tools run
//! with every prompt off and a time limit, and a source that needs a login answers
//! [`SourceError::NotSignedIn`], which the caller turns into a clear refusal and a "Sign in"
//! notice. [`SecretCache`] keeps values in memory with an expiry; [`discover`] lists the signed-in
//! accounts (names only) from a fixed list of commands.
//!
//! A [`Credential`] is a value an identity owns: a source says nothing about how many credentials a
//! workspace has. The same [`Fetch`] call serves the proxy's injection and host-side API calls.
//! Signing in is the user's click and nothing else ([`SignIns`]): `gh auth login --web`, or Git Credential
//! Manager allowed to open its own window. Another source (the Azure CLI) is one more [`SourceSpec`] variant and one more arm in
//! [`Sources`].

#![forbid(unsafe_code)]

mod cache;
mod choice;
mod chunked;
mod discovery;
mod error;
mod file_vault;
mod fix_step;
mod health;
mod indexed;
mod keyring_store;
mod login_session;
mod migrate;
mod os_store;
mod readlog;
mod run;
mod scheme;
mod secret;
mod signin;
mod sources;
mod store;
mod vault_key;

pub use cache::{SecretCache, SignInNeeded, Ttl, Ttls};
pub use choice::{
    ChoiceError, Decision, Inputs, RecordError, Recording, Rule, StoreKind, StoreOption,
    StoreRecord, decide, file_stamp, read_record, secrets_folder, vault_path, write_record,
};
pub use chunked::{CHUNK_UNITS, ChunkedStore};
pub use discovery::{DiscoveredAccount, Discovery, Listing, discover};
pub use error::{Refusal, SourceError, Tool};
pub use file_vault::{FileVault, KeySourceTag, VaultError, VaultKey};
pub use fix_step::fix_step;
pub use health::{StoreAdvice, StoreHealth, probe_os_store, probe_store};
pub use indexed::IndexedStore;
pub use keyring_store::{KeyringStore, keyring_available};
pub use login_session::{LoginSession, login_session, login_user_id};
pub use migrate::{Cleared, MigrateError, Moved, clear_old_store, move_values};
pub use os_store::reconnect_os_store;
pub use puddle_types::{AZURE_DEVOPS_HOST, SourceSpec, TokenScope};
pub use puddle_types::{
    AccountName, HostName, HostNameLit, NameError, OrgName, StoredId, TenantId, UrlPath, host_name,
};
pub use readlog::{LastRead, ReadLog, ReadUse};
pub use run::ToolPaths;
pub use scheme::{TokenScheme, azure_devops_scheme};
pub use secret::Secret;
pub use signin::{SignInError, SignInStart, SignIns};
pub use sources::{Credential, Fetch, Fetched, Sources, pasted_token};
pub use store::{MemoryStore, SecretStore, StoreError, Sweep};
pub use vault_key::{
    FoundKey, KeyError, KeySource, Unseal, VaultKeys, classify_unseal_failure, config_home,
    sealed_key_name, sealed_key_path, systemd_lacks_user_sealing,
};
