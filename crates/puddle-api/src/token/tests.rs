// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn tokens_are_random_hex_and_redacted() {
    let a = ApiToken::generate().unwrap();
    let b = ApiToken::generate().unwrap();
    assert_ne!(a.expose(), b.expose());
    assert_eq!(a.expose().len(), 64);
    assert!(ApiToken::parse(a.expose().to_owned()).is_ok());
    assert_eq!(format!("{a:?}"), "ApiToken(<redacted>)");
    let info = ConnectionInfo {
        url: "http://127.0.0.1:1".into(),
        token: a.clone(),
    };
    assert!(!format!("{info:?}").contains(a.expose()));
}

#[test]
fn matches_only_the_exact_token() {
    let t = ApiToken::generate().unwrap();
    assert!(t.matches(t.expose().as_bytes()));
    assert!(!t.matches(t.expose().to_uppercase().as_bytes()));
    assert!(!t.matches(&t.expose().as_bytes()[..63]));
    assert!(!t.matches(b""));
    let mut longer = t.expose().to_owned();
    longer.push('0');
    assert!(!t.matches(longer.as_bytes()));
}

#[test]
fn hex_encodes_every_nibble() {
    assert_eq!(hex(&[0x00, 0x9f, 0xff, 0x10]), "009fff10");
}

#[test]
fn parse_rejects_malformed_tokens() {
    for bad in ["", "abc", &"g".repeat(64), &"A".repeat(64), &"a".repeat(65)] {
        assert!(ApiToken::parse(bad.to_owned()).is_err(), "{bad}");
    }
}

#[test]
fn write_then_read_round_trips_and_replaces() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sub").join("api.json");
    let first = ConnectionInfo {
        url: "http://127.0.0.1:1".into(),
        token: ApiToken::generate().unwrap(),
    };
    first.write(&path).unwrap();
    let second = ConnectionInfo {
        url: "http://127.0.0.1:2".into(),
        token: ApiToken::generate().unwrap(),
    };
    second.write(&path).unwrap();
    let back = ConnectionInfo::read(&path).unwrap();
    assert_eq!(back.url, second.url);
    assert_eq!(back.token.expose(), second.token.expose());
    // Only the file itself is left: no temporary files.
    let names: Vec<_> = fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, ["api.json"]);
}

#[cfg(unix)]
#[test]
fn file_and_new_directory_are_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("new").join("api.json");
    ConnectionInfo {
        url: "http://127.0.0.1:1".into(),
        token: ApiToken::generate().unwrap(),
    }
    .write(&path)
    .unwrap();
    let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&path), 0o600);
    assert_eq!(mode(path.parent().unwrap()), 0o700);
}

#[cfg(unix)]
#[test]
fn a_file_others_can_read_is_refused() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("api.json");
    let token = ApiToken::generate().unwrap();
    ConnectionInfo {
        url: "http://127.0.0.1:1".into(),
        token: token.clone(),
    }
    .write(&path)
    .unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    let err = ConnectionInfo::read(&path).unwrap_err();
    assert!(
        matches!(
            err,
            ConnectionFileError::Permissions {
                exposed: puddle_fs::private::Exposed::Mode(0o644),
                ..
            }
        ),
        "{err}"
    );
    assert!(!err.to_string().contains(token.expose()));
}

#[cfg(windows)]
#[test]
fn a_file_with_an_inherited_acl_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("api.json");
    let token = ApiToken::generate().unwrap();
    ConnectionInfo {
        url: "http://127.0.0.1:1".into(),
        token: token.clone(),
    }
    .write(&path)
    .unwrap();
    // The same content under the folder's inherited ACL (user, administrators, SYSTEM).
    let copy = dir.path().join("copy.json");
    fs::write(&copy, fs::read(&path).unwrap()).unwrap();
    ConnectionInfo::read(&path).unwrap();
    let err = ConnectionInfo::read(&copy).unwrap_err();
    assert!(
        matches!(
            err,
            ConnectionFileError::Permissions {
                exposed: puddle_fs::private::Exposed::Acl(_),
                ..
            }
        ),
        "{err}"
    );
    assert!(!err.to_string().contains(token.expose()));
}

#[test]
fn malformed_files_are_refused_without_quoting_them() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("api.json");
    let secretish = "x".repeat(64);
    for (body, want) in [
        (
            format!("{{\"token\":\"{secretish}\""),
            "not a connection file",
        ),
        (
            format!(r#"{{"version":2,"url":"u","token":"{}"}}"#, "a".repeat(64)),
            "version 2 is not supported",
        ),
        (
            format!(r#"{{"version":1,"url":"u","token":"{secretish}"}}"#),
            "64 lower-case hex",
        ),
        (" ".repeat(5000), "too large"),
    ] {
        puddle_fs::private::write_atomic(&path, body.as_bytes()).unwrap();
        let err = ConnectionInfo::read(&path).unwrap_err().to_string();
        assert!(err.contains(want), "{err}");
        assert!(!err.contains(&secretish), "{err}");
    }
    let missing = ConnectionInfo::read(&dir.path().join("nope.json")).unwrap_err();
    assert!(matches!(missing, ConnectionFileError::Io { .. }));
}

#[test]
fn write_into_an_unwritable_place_fails_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let blocker = dir.path().join("file");
    fs::write(&blocker, b"").unwrap();
    let info = ConnectionInfo {
        url: "http://127.0.0.1:1".into(),
        token: ApiToken::generate().unwrap(),
    };
    // The "directory" is a file: creating it fails.
    let err = info.write(&blocker.join("api.json")).unwrap_err();
    assert!(matches!(err, ConnectionFileError::Io { .. }), "{err}");
    // The target is a directory: the rename fails and the temporary file is removed.
    let target = dir.path().join("taken");
    fs::create_dir(&target).unwrap();
    fs::write(target.join("keep"), b"").unwrap();
    assert!(info.write(&target).is_err());
    let left: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".tmp"))
        .collect();
    assert!(left.is_empty(), "{left:?}");
}
