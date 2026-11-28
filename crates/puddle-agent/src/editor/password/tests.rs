// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use crate::editor::tests::server;

#[test]
fn passwords_are_long_hex_and_different_every_time() {
    let a = new_password().unwrap();
    let b = new_password().unwrap();
    assert_eq!(a.len(), PASSWORD_BYTES * 2);
    assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
    assert_ne!(a, b);
}

#[test]
fn only_a_password_this_module_wrote_is_reused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pw");
    assert_eq!(existing_password(&path), None);
    let good = new_password().unwrap();
    for (case, bad) in [
        "",
        "short",
        &"g".repeat(64),
        &format!("{good}\n"),
        &format!("{good}00"),
    ]
    .into_iter()
    .enumerate()
    {
        std::fs::write(&path, bad).unwrap();
        assert_eq!(
            existing_password(&path),
            None,
            "a malformed password file (case {case}) was reused"
        );
    }
    std::fs::write(&path, &good).unwrap();
    assert_eq!(existing_password(&path), Some(good));
}

#[test]
fn a_password_is_kept_within_its_boot_and_replaced_in_the_next() {
    let dir = tempfile::tempdir().unwrap();
    let config = EditorConfig {
        server: server(),
        password_file: dir.path().join("run/editor-password"),
        boot_id: dir.path().join("boot_id"),
        root: dir.path().join("root"),
    };
    std::fs::write(&config.boot_id, "boot-one\n").unwrap();
    let first = boot_password(&config).unwrap();
    assert_eq!(
        std::fs::read_to_string(&config.password_file).unwrap(),
        first
    );
    // A restarted server or agent in the same boot: the same password.
    assert_eq!(boot_password(&config).unwrap(), first);
    // The next boot finds the file on the disk and makes a new one.
    std::fs::write(&config.boot_id, "boot-two\n").unwrap();
    let second = boot_password(&config).unwrap();
    assert_ne!(second, first);
    assert_eq!(
        std::fs::read_to_string(&config.password_file).unwrap(),
        second
    );
    assert_eq!(boot_password(&config).unwrap(), second);
    // A password with no stamp (written before the stamp existed) is not kept.
    std::fs::remove_file(boot_stamp(&config.password_file)).unwrap();
    assert_ne!(boot_password(&config).unwrap(), second);
    // A boot that cannot be named keeps nothing.
    std::fs::remove_file(&config.boot_id).unwrap();
    let unnamed = boot_password(&config).unwrap();
    assert_ne!(boot_password(&config).unwrap(), unnamed);
}

#[test]
fn the_password_file_is_owner_only_and_replaced_whole() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run/editor-password");
    let first = puddle_types::fixture::password("first");
    let second = puddle_types::fixture::password("second");
    write_password(&path, &first).unwrap();
    write_password(&path, &second).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), second);
    assert_eq!(
        std::fs::read_dir(dir.path().join("run")).unwrap().count(),
        1
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}

#[cfg(unix)]
#[test]
fn a_password_file_that_was_wider_is_owner_only_after_the_write() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("editor-password");
    std::fs::write(&path, "old").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    write_password(&path, &puddle_types::fixture::password("new")).unwrap();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}
