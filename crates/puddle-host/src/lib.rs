// SPDX-License-Identifier: AGPL-3.0-or-later
//! The Puddle host process as a library: the one place that wires the store, the proxies, the
//! sandbox runtime, workspaces, shutdown and the API together.
//!
//! `puddle serve` and the desktop app both start it the same way:
//!
//! ```no_run
//! # async fn demo(config: puddle_host::HostConfig) -> Result<(), puddle_host::HostError> {
//! use puddle_host::{Host, HostOptions, MsbFactory, SystemPlatform, prepare};
//!
//! // Synchronous, before the process has a second thread: locks the data folder (a second
//! // process on it is refused), binds the pull proxy, pins the
//! // environment, checks the runtime, reads the corporate roots.
//! let prepared = prepare(config, &SystemPlatform::new())?;
//! // Asynchronous: store, upstream chain, runtime, reconcile, proxies, workspaces, API.
//! let host = Host::start(prepared, &MsbFactory, HostOptions::default()).await?;
//! println!("{}", host.url());
//! // ... wait for Ctrl-C / the quit sequence ...
//! host.shutdown().await;
//! # Ok(()) }
//! ```
//!
//! The order of both sequences is part of the contract; see [`Step`], [`START_STEPS`] and
//! [`SHUTDOWN_STEPS`].
#![forbid(unsafe_code)]

mod accounts;
mod agent_reports;
mod backup;
mod boot;
mod changes;
mod company_signals;
mod config;
mod data_folder;
mod dev_cert;
mod disk_watch;
mod doctor;
mod editor_file;
mod editor_updates;
mod environment;
mod error;
mod files;
mod front;
mod git_hosts;
mod host;
mod injection;
mod kubernetes;
mod learned;
mod logins;
mod merge_notice;
mod op_error;
mod paths;
mod platform;
mod ports;
mod proxy_passwords;
mod proxy_sign_ins;
mod quit_trims;
mod settings_read;
mod shell_init;
mod space_watch;
mod ssh_accounts;
mod ssh_agent;
mod ssh_config;
mod ssh_login;
mod ssh_target;
mod terminals;
mod vault;
pub use vault::SecretSetup;
mod vscode;
mod workspaces;

pub use accounts::{AccountError, reset_user};
pub use backup::{
    BACKUP_DATABASE, BACKUP_SETTINGS, BACKUP_WORKSPACES, BackupError, BackupReport, backup,
};
pub use company_signals::{HostSignals, PacReader, PacTiming};
pub use config::{
    AGENT_FILE_NAME, ApiSettings, CODE_SERVER_DIR_NAME, FreeSpaceTiming, GuestSettings, HostConfig,
    RemoteSettings, UpstreamSettings,
};
pub use dev_cert::{DevCertSource, GuestCertSource};
pub use doctor::{disk_limits, ssh_agent_pipe};
pub use error::HostError;
pub use files::FileSettings;
pub use host::{
    Host, HostOptions, HostShutdown, MsbFactory, PREPARE_STEPS, Prepared, RemoteListener,
    RuntimeFactory, RuntimeInputs, SHUTDOWN_STEPS, START_STEPS, Step, prepare,
};
pub use injection::{InjectorFactory, InjectorInputs};
pub use paths::HostPaths;
pub use platform::{Platform, SystemPlatform};
pub use proxy_passwords::{ProxyPasswordError, ProxyPasswords};
pub use space_watch::Space;
pub use ssh_config::{SshSettings, ssh_host};
pub use vscode::VsCodeLauncher;
pub use workspaces::{EndExtensionHostsError, HostWorkspaces, RewriteGate, SwitchEditorBuildError};
