// SPDX-License-Identifier: GPL-3.0-or-later
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use puddle_store::{Actor, Effect, Limits, NewRule, Pattern, Scope, Store, SystemClock};

use super::*;

const CANARY: &str = "CANARY-backup-credential-value";

fn open(path: &Path) -> Store {
    Store::open(path, Arc::new(SystemClock), Limits::default()).unwrap()
}

fn add(store: &Store, n: usize) {
    store
        .add_rule(&NewRule {
            scope: Scope::Global,
            pattern: Pattern::parse(&format!("host{n}.example.org")).unwrap(),
            effect: Effect::Allow,
            expires_at: None,
            created_by: Actor::Cli,
        })
        .unwrap();
}

/// A data folder as a host leaves it, with files that must stay out of a backup.
fn data_folder(root: &Path) -> HostPaths {
    let paths = HostPaths::new(root.join("data"));
    fs::create_dir_all(paths.settings()).unwrap();
    fs::write(paths.settings().join("global.json"), br#"{"version":1}"#).unwrap();
    fs::create_dir_all(paths.settings().join("workspaces")).unwrap();
    fs::write(paths.settings().join("workspaces").join("a.json"), b"{}").unwrap();
    fs::write(paths.settings().join(".global.json.1234.tmp"), b"half").unwrap();
    fs::write(paths.workspace_book(), br#"{"version":2,"workspaces":[]}"#).unwrap();
    fs::create_dir_all(paths.login_markers()).unwrap();
    fs::write(paths.login_markers().join("a"), CANARY).unwrap();
    fs::create_dir_all(paths.front_keys()).unwrap();
    fs::write(paths.front_keys().join("a.key"), CANARY).unwrap();
    fs::write(paths.connection_file(), CANARY).unwrap();
    fs::write(paths.data().join("host.lock"), b"").unwrap();
    paths
}

fn files_below(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            out.extend(files_below(&path));
        } else {
            out.push(path);
        }
    }
    out
}

#[test]
fn backup_restores_to_an_equal_store_and_leaves_credentials_out() {
    let root = tempfile::tempdir().unwrap();
    let paths = data_folder(root.path());
    let live = open(&paths.store());
    for n in 0..5 {
        add(&live, n);
    }
    let dest = root.path().join("out");
    let report = backup(&paths, &dest).unwrap();
    assert_eq!(
        report.settings_files, 2,
        "the half-written temp file stays out"
    );
    assert!(report.workspace_list);

    // Hostile case: nothing credential-shaped reaches the destination, by name or content.
    let holders: Vec<_> = files_below(&dest)
        .into_iter()
        .filter(|f| String::from_utf8_lossy(&fs::read(f).unwrap()).contains(CANARY))
        .collect();
    assert!(holders.is_empty(), "{holders:?} hold the canary");
    let names: Vec<_> = files_below(&dest)
        .iter()
        .map(|p| p.strip_prefix(&dest).unwrap().to_path_buf())
        .collect();
    assert!(
        names.iter().all(|n| {
            let s = n.to_string_lossy();
            !s.contains("login-markers")
                && !s.contains("front")
                && !s.contains("api.json")
                && !s.contains(".lock")
                && !s.ends_with("-wal")
                && !s.ends_with("-shm")
                && !s.ends_with(".tmp")
        }),
        "{names:?}"
    );

    // Opening the copy makes its -wal/-shm, so it comes after the listing above.
    let restored = open(&dest.join(BACKUP_DATABASE));
    assert_eq!(
        restored.rules_every_user(&puddle_store::testing::all_users()),
        live.rules_every_user(&puddle_store::testing::all_users())
    );
    assert_eq!(
        restored.audit_lines(0, 1000).unwrap(),
        live.audit_lines(0, 1000).unwrap()
    );
    assert_eq!(
        fs::read(dest.join(BACKUP_WORKSPACES)).unwrap(),
        fs::read(paths.workspace_book()).unwrap()
    );
}

#[test]
fn backup_while_the_store_is_being_written_is_one_consistent_point() {
    let root = tempfile::tempdir().unwrap();
    let paths = data_folder(root.path());
    let live = Arc::new(open(&paths.store()));
    add(&live, 0);
    let stop = Arc::new(AtomicBool::new(false));
    let writer = {
        let (live, stop) = (Arc::clone(&live), Arc::clone(&stop));
        std::thread::spawn(move || {
            let mut n = 1;
            while !stop.load(Ordering::Relaxed) {
                add(&live, n);
                n += 1;
            }
            n
        })
    };
    // Wait until writes are flowing, then copy repeatedly while they continue.
    puddle_test_kit::eventually_blocking(
        "the writer has committed 20 rules",
        std::time::Duration::from_secs(10),
        || {
            live.rules_every_user(&puddle_store::testing::all_users())
                .len()
                >= 20
        },
    );
    let mut copies = Vec::new();
    for i in 0..5 {
        let dest = root.path().join(format!("out{i}"));
        let before = live
            .rules_every_user(&puddle_store::testing::all_users())
            .len();
        backup(&paths, &dest).unwrap();
        copies.push((dest, before));
    }
    stop.store(true, Ordering::Relaxed);
    let total = writer.join().unwrap();

    let final_rules = live.rules_every_user(&puddle_store::testing::all_users());
    assert_eq!(final_rules.len(), total);
    for (dest, before) in copies {
        let restored = open(&dest.join(BACKUP_DATABASE));
        let rules = restored.rules_every_user(&puddle_store::testing::all_users());
        // A prefix of the live history: every rule the host had committed before the copy
        // began is in it, and it matches the live store row for row up to its own length.
        assert!(rules.len() >= before, "{} < {before}", rules.len());
        assert!(rules.len() <= final_rules.len());
        let live_prefix: Vec<_> = final_rules.iter().take(rules.len()).cloned().collect();
        assert_eq!(rules, live_prefix);
        // The audit log is a prefix too, with no gap.
        let lines = restored.audit_lines(0, 100_000).unwrap();
        let live_lines = live.audit_lines(0, 100_000).unwrap();
        assert_eq!(lines[..], live_lines[..lines.len()]);
    }
}

#[test]
fn existing_destination_is_refused_and_untouched() {
    let root = tempfile::tempdir().unwrap();
    let paths = data_folder(root.path());
    drop(open(&paths.store()));
    let dest = root.path().join("out");
    fs::create_dir(&dest).unwrap();
    fs::write(dest.join("mine"), b"keep").unwrap();
    assert!(matches!(
        backup(&paths, &dest),
        Err(BackupError::DestinationExists(_))
    ));
    assert_eq!(fs::read(dest.join("mine")).unwrap(), b"keep");
}

#[test]
fn destination_inside_the_data_folder_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let paths = data_folder(root.path());
    drop(open(&paths.store()));
    let dest = paths.data().join("settings").join("out");
    assert!(matches!(
        backup(&paths, &dest),
        Err(BackupError::InsideData(_))
    ));
    assert!(!dest.exists());
}

#[test]
fn missing_database_is_refused_and_creates_nothing() {
    let root = tempfile::tempdir().unwrap();
    let paths = HostPaths::new(root.path().join("data"));
    let dest = root.path().join("out");
    assert!(matches!(
        backup(&paths, &dest),
        Err(BackupError::NoDatabase(_))
    ));
    assert!(!dest.exists());
    assert!(!paths.data().exists(), "the data folder is not created");
}

#[test]
fn failed_copy_leaves_no_folder_behind() {
    let root = tempfile::tempdir().unwrap();
    let paths = HostPaths::new(root.path().join("data"));
    fs::create_dir_all(paths.data()).unwrap();
    fs::write(paths.store(), b"this is not a database").unwrap();
    let dest = root.path().join("out");
    assert!(matches!(
        backup(&paths, &dest),
        Err(BackupError::Database(_))
    ));
    assert!(!dest.exists());
}

#[cfg(unix)]
#[test]
fn backup_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let paths = data_folder(root.path());
    drop(open(&paths.store()));
    let dest = root.path().join("out");
    backup(&paths, &dest).unwrap();
    for path in files_below(&dest).into_iter().chain([dest.clone()]) {
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o077;
        assert_eq!(mode, 0, "{} is open to others", path.display());
    }
}

#[test]
fn missing_settings_and_book_are_fine() {
    let root = tempfile::tempdir().unwrap();
    let paths = HostPaths::new(root.path().join("data"));
    fs::create_dir_all(paths.data()).unwrap();
    drop(open(&paths.store()));
    let report = backup(&paths, &root.path().join("out")).unwrap();
    assert_eq!(report.settings_files, 0);
    assert!(!report.workspace_list);
}

#[test]
fn unreadable_workspace_list_fails_the_backup_and_leaves_no_folder() {
    let root = tempfile::tempdir().unwrap();
    let paths = data_folder(root.path());
    drop(open(&paths.store()));
    // A folder where the list should be: reading it fails with something other than "missing".
    fs::remove_file(paths.workspace_book()).unwrap();
    fs::create_dir(paths.workspace_book()).unwrap();
    let dest = root.path().join("out");
    assert!(matches!(backup(&paths, &dest), Err(BackupError::Io { .. })));
    assert!(!dest.exists());
}

#[test]
fn unreadable_settings_folder_fails_the_backup_and_leaves_no_folder() {
    let root = tempfile::tempdir().unwrap();
    let paths = data_folder(root.path());
    drop(open(&paths.store()));
    // A file where the folder should be.
    fs::remove_dir_all(paths.settings()).unwrap();
    fs::write(paths.settings(), b"not a folder").unwrap();
    let dest = root.path().join("out");
    assert!(matches!(backup(&paths, &dest), Err(BackupError::Io { .. })));
    assert!(!dest.exists());
}

#[test]
fn destination_under_a_file_cannot_be_created() {
    let root = tempfile::tempdir().unwrap();
    let paths = data_folder(root.path());
    drop(open(&paths.store()));
    let blocker = root.path().join("blocker");
    fs::write(&blocker, b"a file").unwrap();
    let err = backup(&paths, &blocker.join("out")).unwrap_err();
    assert!(matches!(err, BackupError::Io { .. }), "{err}");
}
