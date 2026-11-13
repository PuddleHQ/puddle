// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn denied() -> io::Error {
    io::Error::from(io::ErrorKind::PermissionDenied)
}

#[test]
fn a_denial_that_ends_with_the_file_gone_is_nothing_to_tighten() {
    let mut calls = 0;
    let result = tighten_file_within(Duration::from_secs(5), || {
        calls += 1;
        Err(if calls < 3 {
            denied()
        } else {
            io::Error::from(io::ErrorKind::NotFound)
        })
    });
    assert_eq!(result.unwrap(), None);
    assert_eq!(calls, 3);
}

#[test]
fn a_file_deleted_by_another_thread_while_denied_is_nothing_to_tighten() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("puddle.db-wal");
    fs::write(&path, b"x").unwrap();
    let (denied_once, denied_once_rx) = std::sync::mpsc::channel();
    let deleter = {
        let path = path.clone();
        thread::spawn(move || {
            // The delete lands only after the tighten step was denied once, so it is retrying.
            denied_once_rx.recv().unwrap();
            fs::remove_file(path).unwrap();
        })
    };
    // The file exists and is denied until it is deleted, then it is missing.
    let result = tighten_file_within(Duration::from_secs(10), || {
        Err(if path.exists() {
            let _ = denied_once.send(());
            denied()
        } else {
            io::Error::from(io::ErrorKind::NotFound)
        })
    });
    deleter.join().unwrap();
    assert_eq!(result.unwrap(), None);
}

#[test]
fn a_denial_that_goes_away_with_the_file_still_there_is_tightened() {
    let mut calls = 0;
    let result = tighten_file_within(Duration::from_secs(5), || {
        calls += 1;
        if calls < 2 {
            Err(denied())
        } else {
            Ok(Some(Exposed::Mode(0o644)))
        }
    });
    assert_eq!(result.unwrap(), Some(Exposed::Mode(0o644)));
}

#[test]
fn a_denial_that_lasts_is_returned_after_the_grace_period() {
    let grace = Duration::from_millis(60);
    let started = Instant::now();
    let mut calls = 0;
    let err = tighten_file_within(grace, || {
        calls += 1;
        Err(denied())
    })
    .unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    assert!(started.elapsed() >= grace);
    assert!(calls > 1, "a denial is retried, not returned at once");
}

#[test]
fn other_errors_are_returned_at_once() {
    let mut calls = 0;
    let err = tighten_file_within(Duration::from_secs(5), || {
        calls += 1;
        Err(io::Error::from(io::ErrorKind::InvalidInput))
    })
    .unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(calls, 1);
}

#[test]
fn a_missing_file_is_nothing_to_tighten() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(tighten_file(&dir.path().join("none")).unwrap(), None);
}

#[test]
fn write_atomic_creates_folders_replaces_and_leaves_no_temporary_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sub").join("secret.json");
    write_atomic(&path, b"one").unwrap();
    write_atomic(&path, b"two").unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"two");
    let names: Vec<_> = fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, ["secret.json"]);
    check(&File::open(&path).unwrap()).unwrap();
}

#[test]
fn write_atomic_cleans_up_when_the_rename_fails() {
    let dir = tempfile::tempdir().unwrap();
    // A folder at the target makes the rename fail after the temporary file was written.
    let path = dir.path().join("target");
    fs::create_dir(&path).unwrap();
    fs::write(path.join("keep"), b"x").unwrap();
    assert!(write_atomic(&path, b"secret").is_err());
    let names: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, ["target"]);
}

#[test]
fn create_file_refuses_an_existing_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f");
    drop(create_file(&path).unwrap());
    assert_eq!(
        create_file(&path).unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
}

#[test]
fn create_new_dir_makes_one_owner_only_folder_and_refuses_an_existing_one() {
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("fresh");
    create_new_dir(&sub).unwrap();
    assert!(sub.is_dir());
    // Already owner-only: nothing to tighten (an ACL check on Windows, mode bits on Unix).
    assert_eq!(tighten_dir(&sub).unwrap(), None);
    assert_eq!(
        create_new_dir(&sub).unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
}

#[test]
fn create_new_dir_does_not_create_parents() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        create_new_dir(&dir.path().join("a").join("b"))
            .unwrap_err()
            .kind(),
        io::ErrorKind::NotFound
    );
}

#[test]
fn create_dir_keeps_an_existing_folder() {
    let dir = tempfile::tempdir().unwrap();
    create_dir(dir.path()).unwrap();
    create_dir(&dir.path().join("a").join("b")).unwrap();
    assert!(dir.path().join("a").join("b").is_dir());
}

#[test]
fn tighten_creates_a_missing_folder_and_skips_a_missing_file() {
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("new");
    assert_eq!(tighten_dir(&sub).unwrap(), None);
    assert!(sub.is_dir());
    assert_eq!(tighten_file(&sub.join("absent")).unwrap(), None);
}

#[test]
fn a_file_that_goes_away_while_it_is_tightened_is_skipped() {
    use std::sync::atomic::{AtomicBool, Ordering};

    // SQLite deletes its `-wal` file when its last connection closes, which can happen between
    // the moment the file is seen and the moment it is opened.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("puddle.db-wal");
    let stop = AtomicBool::new(false);
    thread::scope(|scope| {
        scope.spawn(|| {
            while !stop.load(Ordering::Relaxed) {
                drop(create_file(&path));
                let _ = fs::remove_file(&path);
            }
        });
        // Stop the churn before judging, so a failure ends the test instead of hanging it.
        // Only `NotFound` is judged here; the "access denied" Windows answers for a file in
        // the middle of being deleted has its own tests.
        let deadline = Instant::now() + Duration::from_millis(500);
        let mut first_error = None;
        while first_error.is_none() && Instant::now() < deadline {
            first_error = tighten_file(&path)
                .err()
                .filter(|err| err.kind() == io::ErrorKind::NotFound);
        }
        stop.store(true, Ordering::Relaxed);
        assert!(first_error.is_none(), "{first_error:?}");
    });
}

#[test]
fn tighten_leaves_an_owner_only_folder_and_file_alone() {
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("own");
    create_dir(&sub).unwrap();
    let file = sub.join("f");
    drop(create_file(&file).unwrap());
    assert_eq!(tighten_dir(&sub).unwrap(), None);
    assert_eq!(tighten_file(&file).unwrap(), None);
}

#[test]
fn tighten_refuses_a_file_where_a_folder_is_expected_and_the_reverse() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("f");
    fs::write(&file, b"x").unwrap();
    assert_eq!(
        tighten_dir(&file).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    // A folder inside the temp folder, so a `tighten_file` that wrongly chmods what it is given
    // cannot leave the temp folder itself unremovable.
    let sub = dir.path().join("sub");
    fs::create_dir(&sub).unwrap();
    assert_eq!(
        tighten_file(&sub).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
}

#[test]
fn hex_is_two_digits_per_byte() {
    assert_eq!(hex(&[0x00, 0x9f, 0xff, 0x10]), "009fff10");
}

#[test]
fn replacing_a_file_puts_the_new_content_in_place_and_removes_the_copy() {
    let dir = tempfile::tempdir().unwrap();
    let to = dir.path().join("config");
    fs::write(&to, b"old").unwrap();
    let from = dir.path().join("config.tmp");
    let mut file = create_file(&from).unwrap();
    file.write_all(b"new").unwrap();
    drop(file);
    replace_file(&from, &to).unwrap();
    assert_eq!(fs::read(&to).unwrap(), b"new");
    assert!(!from.exists());
}

// A rename makes the file on Unix; `ReplaceFileW` wants the target to exist.
#[cfg(windows)]
#[test]
fn replacing_a_file_that_is_not_there_is_an_error_and_keeps_the_copy() {
    let dir = tempfile::tempdir().unwrap();
    let from = dir.path().join("config.tmp");
    drop(create_file(&from).unwrap());
    assert!(replace_file(&from, &dir.path().join("missing")).is_err());
    assert!(from.exists());
}

/// The access list of `path` as `icacls` prints it, without the path.
#[cfg(windows)]
fn acl_of(path: &Path) -> Vec<String> {
    let out = std::process::Command::new("icacls")
        .arg(path)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let path = path.display().to_string();
    let mut entries: Vec<String> = text
        .lines()
        .map(|l| l.replace(&path, "").trim().to_owned())
        .filter(|l| !l.is_empty() && !l.starts_with("Successfully processed"))
        .collect();
    entries.sort();
    entries
}

#[cfg(windows)]
#[test]
fn replacing_a_file_keeps_the_access_list_it_had_on_windows() {
    let dir = tempfile::tempdir().unwrap();
    let make = |name: &str| {
        let to = dir.path().join(name);
        fs::write(&to, b"old").unwrap();
        // An access list that differs from the owner-only one the copy is made with.
        let granted = std::process::Command::new("icacls")
            .arg(&to)
            .args(["/grant", "*S-1-1-0:(R)"])
            .output()
            .unwrap();
        assert!(granted.status.success());
        let from = dir.path().join(format!("{name}.tmp"));
        create_file(&from).unwrap().write_all(b"new").unwrap();
        (from, to)
    };
    let (from, to) = make("kept");
    let before = acl_of(&to);
    assert!(before.iter().any(|e| e.contains("Everyone")), "{before:?}");
    replace_file(&from, &to).unwrap();
    assert_eq!(fs::read(&to).unwrap(), b"new");
    assert_eq!(acl_of(&to), before);
    // The control: a plain rename takes the access list of the copy, which is the loss
    // `replace_file` exists to avoid.
    let (from, to) = make("renamed");
    let before = acl_of(&to);
    fs::rename(&from, &to).unwrap();
    assert_ne!(acl_of(&to), before);
}

#[test]
fn what_a_file_exposes_is_said_in_words() {
    assert_eq!(
        Exposed::Mode(0o644).to_string(),
        "mode 644; it must be 0600"
    );
    assert_eq!(
        Exposed::Acl("Everyone may read".to_owned()).to_string(),
        "Everyone may read; it must be accessible to its owner only"
    );
    assert_eq!(
        CheckError::Exposed(Exposed::Mode(0o640)).to_string(),
        "readable by other users (mode 640; it must be 0600)"
    );
}

#[test]
fn check_passes_an_owner_only_file() {
    let dir = tempfile::tempdir().unwrap();
    let file = create_file(&dir.path().join("private")).unwrap();
    check(&file).unwrap();
}

#[cfg(unix)]
#[test]
fn check_refuses_a_file_others_can_read() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("open");
    fs::write(&path, b"x").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    let err = check(&File::open(&path).unwrap()).unwrap_err();
    assert!(
        matches!(err, CheckError::Exposed(Exposed::Mode(0o644))),
        "{err}"
    );
}
