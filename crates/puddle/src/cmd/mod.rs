// SPDX-License-Identifier: AGPL-3.0-or-later
//! One module per subcommand that does more than print a line.

pub mod audit;
pub mod backup;
pub mod data_folder;
pub mod doctor;
pub mod secrets;
pub mod serve;
pub mod ssh_bridge;
pub mod user;

use puddle_host::{AccountError, BackupError, VaultAdminError};

use self::serve::config_file::ConfigError;

/// Why an admin subcommand ended in failure. It displays as the line `main` prints before
/// exiting 1.
#[derive(Debug, thiserror::Error)]
pub(crate) enum CommandError {
    /// The data folder option or the config file is not usable.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// No per-user data folder could be found.
    #[error(transparent)]
    DataDir(#[from] puddle_fs::DataDirError),
    /// A user or audit operation failed.
    #[error(transparent)]
    Account(#[from] AccountError),
    /// The backup failed.
    #[error(transparent)]
    Backup(#[from] BackupError),
    /// The working folder is unknown.
    #[error("{0}")]
    WorkingDirectory(std::io::Error),
    /// A vault operation failed.
    #[error(transparent)]
    Vault(#[from] VaultAdminError),
    /// Adding a user needs several users switched on.
    #[error(
        "several users are off (multi_user), so only the owner can sign in and {name} could not \
         use an invite; switch multi_user on first"
    )]
    MultiUserOff {
        /// The user to add.
        name: String,
    },
    /// An invite link needs a public address, and the options give none.
    #[error(
        "remote serving is not on in these options, so there is no public address for the link; \
         give the same --config (or PUDDLE_REMOTE and PUDDLE_PUBLIC_HOSTS) as the server"
    )]
    NoPublicAddress,
}

/// The data folder the options name, else the per-user one.
pub(crate) fn data_folder_here(
    flags: &serve::options::Flags,
) -> Result<puddle_host::HostPaths, CommandError> {
    let data = match data_folder::configured_here(flags)? {
        Some(dir) => dir,
        None => puddle_fs::data_dir()?,
    };
    Ok(puddle_host::HostPaths::new(data))
}
