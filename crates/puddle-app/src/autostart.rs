// SPDX-License-Identifier: GPL-3.0-or-later
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
            match key.get_value::<String, _>(&self.name) {
                Ok(command) => Ok(Some(command)),
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
    mod tests {
        use super::*;

        #[test]
        fn the_real_run_key_is_named_where_windows_reads_it() {
            // Only the name is checked: nothing here reads or writes the user's own list.
            let real = RunKey::user();
            assert_eq!(
                real.subkey,
                r"Software\Microsoft\Windows\CurrentVersion\Run"
            );
            assert_eq!(real.name, "Puddle");
        }
    }
}

/// A start-at-sign-in entry kept in memory, for tests of the code that applies the switch.
#[cfg(test)]
pub(crate) mod memory {
    use std::sync::Mutex;

    use super::{LoginEntry, LoginError};

    /// A list in memory, counting what was changed.
    #[derive(Debug, Default)]
    pub(crate) struct Fake {
        value: Mutex<Option<String>>,
        changes: Mutex<u32>,
    }

    impl Fake {
        pub(crate) fn holding(command: &str) -> Self {
            Self {
                value: Mutex::new(Some(command.to_owned())),
                changes: Mutex::new(0),
            }
        }

        pub(crate) fn now(&self) -> Option<String> {
            self.value.lock().unwrap().clone()
        }

        pub(crate) fn changes(&self) -> u32 {
            *self.changes.lock().unwrap()
        }
    }

    impl LoginEntry for Fake {
        fn command(&self) -> Result<Option<String>, LoginError> {
            Ok(self.now())
        }

        fn set(&self, command: &str) -> Result<(), LoginError> {
            *self.value.lock().unwrap() = Some(command.to_owned());
            *self.changes.lock().unwrap() += 1;
            Ok(())
        }

        fn clear(&self) -> Result<(), LoginError> {
            *self.value.lock().unwrap() = None;
            *self.changes.lock().unwrap() += 1;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::memory::Fake;
    use super::*;

    fn exe() -> PathBuf {
        PathBuf::from("/apps/Puddle/puddle-app")
    }

    #[test]
    fn the_command_quotes_the_program_and_passes_the_flag() {
        assert_eq!(
            command_for(&PathBuf::from("/Program Files/Puddle/puddle-app")),
            "\"/Program Files/Puddle/puddle-app\" --at-login"
        );
    }

    #[test]
    fn turning_it_on_writes_the_entry_once_and_turning_it_off_removes_it() {
        let entry = Fake::default();
        assert_eq!(apply(&entry, true, &exe()).unwrap(), Applied::Written);
        assert_eq!(entry.now(), Some(command_for(&exe())));
        assert_eq!(apply(&entry, true, &exe()).unwrap(), Applied::Unchanged);
        assert_eq!(entry.changes(), 1, "a second apply writes nothing");
        assert_eq!(apply(&entry, false, &exe()).unwrap(), Applied::Removed);
        assert_eq!(entry.now(), None);
        assert_eq!(apply(&entry, false, &exe()).unwrap(), Applied::Unchanged);
        assert_eq!(entry.changes(), 2);
    }

    #[test]
    fn an_entry_that_points_somewhere_else_is_corrected_when_on() {
        let entry = Fake::holding("\"/old/place/puddle-app\" --at-login");
        assert_eq!(apply(&entry, true, &exe()).unwrap(), Applied::Written);
        assert_eq!(entry.now(), Some(command_for(&exe())));
    }

    #[test]
    fn the_flag_is_found_among_the_arguments() {
        assert!(started_at_login(["puddle-app", "--at-login"]));
        assert!(!started_at_login(["puddle-app", "--at-logins"]));
        assert!(!started_at_login(Vec::<String>::new()));
    }

    #[test]
    fn a_system_without_an_implementation_has_no_entry_and_says_so_when_asked_to_make_one() {
        let entry = Unsupported("linux");
        assert_eq!(apply(&entry, false, &exe()).unwrap(), Applied::Unchanged);
        let err = apply(&entry, true, &exe()).unwrap_err();
        assert_eq!(
            err.to_string(),
            "starting Puddle at sign-in is not built for linux yet"
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn the_system_entry_off_this_platform_is_the_unsupported_one() {
        let entry = system();
        assert_eq!(entry.command().unwrap(), None);
        assert!(matches!(
            entry.set("x"),
            Err(LoginError::Unsupported(os)) if os == std::env::consts::OS
        ));
    }

    #[cfg(windows)]
    mod windows {
        use super::*;

        const PARENT: &str = r"Software\PuddleAutostartTest";

        /// A key under `HKEY_CURRENT_USER` of this test's own (named by a fresh temporary folder,
        /// so no two runs share one), removed at the end; never the real `Run` key.
        struct Throwaway {
            key: String,
            _unique: tempfile::TempDir,
        }

        impl Throwaway {
            fn new() -> Self {
                let unique = tempfile::tempdir().unwrap();
                let name = unique
                    .path()
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                Self {
                    key: format!(r"{PARENT}\{name}"),
                    _unique: unique,
                }
            }

            fn entry(&self) -> RunKey {
                RunKey::at(&self.key, ENTRY_NAME)
            }
        }

        impl Drop for Throwaway {
            fn drop(&mut self) {
                let root = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
                let _ = root.delete_subkey_all(&self.key);
                // Fails while another test's key is still under it, which is what is wanted.
                let _ = root.delete_subkey(PARENT);
            }
        }

        #[test]
        fn the_run_key_value_follows_on_and_off() {
            let key = Throwaway::new();
            let entry = key.entry();
            assert_eq!(entry.command().unwrap(), None, "no key yet is no entry");
            assert_eq!(apply(&entry, false, &exe()).unwrap(), Applied::Unchanged);
            assert_eq!(apply(&entry, true, &exe()).unwrap(), Applied::Written);
            assert_eq!(entry.command().unwrap(), Some(command_for(&exe())));
            // The value is really in the registry, read back through a second handle.
            assert_eq!(key.entry().command().unwrap(), Some(command_for(&exe())));
            assert_eq!(apply(&entry, true, &exe()).unwrap(), Applied::Unchanged);
            assert_eq!(apply(&entry, false, &exe()).unwrap(), Applied::Removed);
            assert_eq!(entry.command().unwrap(), None);
            assert_eq!(apply(&entry, false, &exe()).unwrap(), Applied::Unchanged);
        }

        #[test]
        fn removing_leaves_other_values_in_the_key_alone() {
            let key = Throwaway::new();
            let other = RunKey::at(&key.key, "SomethingElse");
            other.set("\"C:\\other.exe\"").unwrap();
            let entry = key.entry();
            apply(&entry, true, &exe()).unwrap();
            apply(&entry, false, &exe()).unwrap();
            assert_eq!(
                other.command().unwrap(),
                Some("\"C:\\other.exe\"".to_owned())
            );
        }
    }
}
