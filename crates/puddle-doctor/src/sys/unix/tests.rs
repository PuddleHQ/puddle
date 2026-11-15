// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn file_access_needs_read_and_execute() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("msb");
    std::fs::write(&f, b"x").unwrap();
    std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        file_access(&f).unwrap_err().kind(),
        io::ErrorKind::PermissionDenied
    );
    // flake-ok: access(2) reads the mode bits; the file is never executed
    std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
    file_access(&f).unwrap();
    assert!(file_access(&dir.path().join("none")).is_err());
}

#[test]
fn windows_only_checks_are_absent() {
    assert_eq!(code_integrity(), None);
    assert_eq!(job(), None);
    assert_eq!(global_secure_access(), None);
    assert_eq!(ssh_agent_service(), None);
}
