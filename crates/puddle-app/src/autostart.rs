// SPDX-License-Identifier: AGPL-3.0-or-later
//! Starting Puddle when the user signs in. The Settings switch is kept with the other window
//! choices; this module turns it into the one thing the operating system keeps: an entry that
//! starts the program at sign-in.
//!
//! The entry sits behind [`LoginEntry`] so the rest of the shell, and its tests, never touch the
//! system's own list. Windows has an implementation (the current user's `Run` key); other systems
//! get [`Unsupported`], which says so when the switch is turned on.
//!
//! The installer removes the Windows entry when Puddle is uninstalled, so no sign-in ever starts a
//! program that is gone (the installer's own test checks it).

use std::path::Path;

/// The argument the sign-in entry passes, so a start at sign-in opens hidden (see
/// [`crate::shell::Options::at_login`]).
pub const AT_LOGIN_FLAG: &str = "--at-login";

/// The name Puddle's entry has in the system's list.
pub const ENTRY_NAME: &str = "Puddle";

/// Why the entry could not be read or changed.
#[derive(Debug, thiserror::Error)]
pub enum LoginError {
    /// This system has no implementation yet.
    #[error("starting Puddle at sign-in is not built for {0} yet")]
    Unsupported(&'static str),
    /// The system refused.
    #[error("the start-at-sign-in entry could not be {action}: {source}")]
    System {
        /// What was tried: `read`, `written` or `removed`.
        action: &'static str,
        /// What the system said.
        source: std::io::Error,
    },
}

/// What a change did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applied {
    /// The entry was already as wanted.
    Unchanged,
    /// The entry was written (or its program path was corrected).
    Written,
    /// The entry was removed.
    Removed,
}

/// The system's start-at-sign-in entry for Puddle.
pub trait LoginEntry: std::fmt::Debug + Send + Sync {
    /// The command the entry runs, `None` when there is no entry.
    ///
    /// # Errors
    ///
    /// [`LoginError`] when the system's list cannot be read.
    fn command(&self) -> Result<Option<String>, LoginError>;

    /// Creates the entry, or replaces the command of an existing one.
    ///
    /// # Errors
    ///
    /// [`LoginError`] when the system refuses, or has no implementation.
    fn set(&self, command: &str) -> Result<(), LoginError>;

    /// Removes the entry; no entry is no error.
    ///
    /// # Errors
    ///
    /// [`LoginError`] when the system refuses.
    fn clear(&self) -> Result<(), LoginError>;
}

/// The command that starts the program at `exe` hidden: its path in quotes, then the flag.
#[must_use]
pub fn command_for(exe: &Path) -> String {
    format!("\"{}\" {AT_LOGIN_FLAG}", exe.display())
}

/// Brings `entry` to the wanted state for the program at `exe`: written (or corrected, after the
/// program moved) when `wanted`, removed when not. Safe to repeat.
///
/// # Errors
///
/// [`LoginError`] from the entry.
pub fn apply(entry: &dyn LoginEntry, wanted: bool, exe: &Path) -> Result<Applied, LoginError> {
    let current = entry.command()?;
    if !wanted {
        return if current.is_some() {
            entry.clear().map(|()| Applied::Removed)
        } else {
            Ok(Applied::Unchanged)
        };
    }
    let command = command_for(exe);
    if current.as_deref() == Some(command.as_str()) {
        return Ok(Applied::Unchanged);
    }
    entry.set(&command).map(|()| Applied::Written)
}

/// Whether `args` (the program's arguments) say it was started by the sign-in entry.
#[must_use]
pub fn started_at_login<S: AsRef<std::ffi::OsStr>>(args: impl IntoIterator<Item = S>) -> bool {
    args.into_iter().any(|arg| arg.as_ref() == AT_LOGIN_FLAG)
}

/// The entry of a system that has no implementation: nothing is ever there, and turning it on
/// says why it does not work.
#[cfg(any(test, not(windows)))]
#[derive(Debug, Clone, Copy)]
pub struct Unsupported(pub &'static str);

#[cfg(any(test, not(windows)))]
impl LoginEntry for Unsupported {
    fn command(&self) -> Result<Option<String>, LoginError> {
        Ok(None)
    }

    fn set(&self, _command: &str) -> Result<(), LoginError> {
        Err(LoginError::Unsupported(self.0))
    }

    fn clear(&self) -> Result<(), LoginError> {
        Ok(())
    }
}

/// The entry of the system Puddle runs on.
#[cfg(windows)]
#[must_use]
pub fn system() -> std::sync::Arc<dyn LoginEntry> {
    std::sync::Arc::new(RunKey::user())
}

/// The entry of the system Puddle runs on.
#[cfg(not(windows))]
#[must_use]
pub fn system() -> std::sync::Arc<dyn LoginEntry> {
    std::sync::Arc::new(Unsupported(std::env::consts::OS))
}

#[cfg(windows)]
pub use run_key::RunKey;

/// The current user's `Run` key: the list Windows starts programs from at sign-in, with no
/// elevation and no effect on other users.
#[cfg(windows)]
mod run_key {
    use std::io::ErrorKind;

    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};
    use winreg::types::FromRegValue;

    use super::{ENTRY_NAME, LoginEntry, LoginError};

    /// The key Windows reads at sign-in.
    const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

    /// One value in a key under `HKEY_CURRENT_USER`.
    #[derive(Debug, Clone)]
    pub struct RunKey {
        subkey: String,
        name: String,
    }

    impl RunKey {
        /// Puddle's entry in the real `Run` key.
        #[must_use]
        pub fn user() -> Self {
            Self::at(RUN, ENTRY_NAME)
        }

        /// A value `name` in the key `subkey` under `HKEY_CURRENT_USER` (tests use a throwaway
        /// key; Windows reads only the real one at sign-in).
        #[must_use]
        pub fn at(subkey: &str, name: &str) -> Self {
            Self {
                subkey: subkey.to_owned(),
                name: name.to_owned(),
            }
        }

        fn failed(action: &'static str) -> impl FnOnce(std::io::Error) -> LoginError {
            move |source| LoginError::System { action, source }
        }
    }

    impl LoginEntry for RunKey {
        fn command(&self) -> Result<Option<String>, LoginError> {
            let root = RegKey::predef(HKEY_CURRENT_USER);
            let key = match root.open_subkey_with_flags(&self.subkey, KEY_READ) {
                Ok(key) => key,
                Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
                Err(err) => return Err(Self::failed("read")(err)),
            };
            match key.get_raw_value(&self.name) {
                // A value of another type is there but is not ours: empty text, which `apply`
                // overwrites when on and removes when off.
                Ok(raw) => Ok(Some(String::from_reg_value(&raw).unwrap_or_default())),
                Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
                Err(err) => Err(Self::failed("read")(err)),
            }
        }

        fn set(&self, command: &str) -> Result<(), LoginError> {
            let (key, _) = RegKey::predef(HKEY_CURRENT_USER)
                .create_subkey_with_flags(&self.subkey, KEY_WRITE)
                .map_err(Self::failed("written"))?;
            key.set_value(&self.name, &command)
                .map_err(Self::failed("written"))
        }

        fn clear(&self) -> Result<(), LoginError> {
            let key = match RegKey::predef(HKEY_CURRENT_USER)
                .open_subkey_with_flags(&self.subkey, KEY_WRITE)
            {
                Ok(key) => key,
                Err(err) if err.kind() == ErrorKind::NotFound => return Ok(()),
                Err(err) => return Err(Self::failed("removed")(err)),
            };
            match key.delete_value(&self.name) {
                Ok(()) => Ok(()),
                Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
                Err(err) => Err(Self::failed("removed")(err)),
            }
        }
    }

    #[cfg(test)]
    mod tests;
}

/// A start-at-sign-in entry kept in memory, for tests of the code that applies the switch.
#[cfg(test)]
pub(crate) mod memory;

#[cfg(test)]
mod tests;
