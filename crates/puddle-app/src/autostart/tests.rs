// SPDX-License-Identifier: AGPL-3.0-or-later
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
    entry.clear().unwrap();
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

    const PREFIX: &str = "PuddleAutostartTest-";

    /// A key directly under `HKEY_CURRENT_USER` of this test's own (named by a fresh temporary
    /// folder, so no two runs share one), removed at the end; never the real `Run` key. There
    /// is no shared parent key: removing a parent while another test creates a key under it
    /// fails that creation with "marked for deletion".
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
                key: format!(r"Software\{PREFIX}{name}"),
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
    fn a_value_of_another_type_is_replaced_when_on_and_removed_when_off() {
        let key = Throwaway::new();
        let (created, _) = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER)
            .create_subkey(&key.key)
            .unwrap();
        created.set_value(ENTRY_NAME, &7_u32).unwrap();
        let entry = key.entry();
        assert_eq!(apply(&entry, true, &exe()).unwrap(), Applied::Written);
        assert_eq!(entry.command().unwrap(), Some(command_for(&exe())));
        created.set_value(ENTRY_NAME, &7_u32).unwrap();
        assert_eq!(apply(&entry, false, &exe()).unwrap(), Applied::Removed);
        assert_eq!(entry.command().unwrap(), None);
    }

    #[test]
    fn throwaway_keys_made_and_dropped_at_the_same_time_never_disturb_each_other() {
        // Every test of this module (and every test process) makes and removes its own key
        // at the same moment: one removal must never invalidate another's creation.
        let workers: Vec<_> = (0..4)
            .map(|_| {
                std::thread::spawn(|| {
                    for _ in 0..300 {
                        let key = Throwaway::new();
                        let (created, _) = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER)
                            .create_subkey(&key.key)
                            .unwrap();
                        created.set_value(ENTRY_NAME, &7_u32).unwrap();
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
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
