// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use crate::MsbConfig;

async fn yes() -> bool {
    true
}

fn probe_timeout() -> MicrosandboxError {
    MicrosandboxError::Runtime("runtime version probe timed out".into())
}

const NO_WAIT: [Duration; 3] = [Duration::from_millis(1); 3];

#[tokio::test]
async fn a_probe_timeout_is_repeated_until_the_host_answers() {
    let calls = std::cell::Cell::new(0_u32);
    let result = retry_probe_timeouts(
        &NO_WAIT,
        || {
            calls.set(calls.get() + 1);
            let n = calls.get();
            async move { if n < 3 { Err(probe_timeout()) } else { Ok(n) } }
        },
        yes,
    )
    .await;
    assert_eq!(result.unwrap(), 3);
    assert_eq!(calls.get(), 3);
}

#[tokio::test]
async fn a_probe_that_never_answers_stops_after_the_last_pause() {
    let calls = std::cell::Cell::new(0_u32);
    let result: Result<(), _> = retry_probe_timeouts(
        &NO_WAIT,
        || {
            calls.set(calls.get() + 1);
            async { Err(probe_timeout()) }
        },
        yes,
    )
    .await;
    assert!(is_probe_timeout(&result.unwrap_err()));
    assert_eq!(calls.get(), 4);
}

#[tokio::test]
async fn other_errors_and_touched_state_are_not_repeated() {
    let calls = std::cell::Cell::new(0_u32);
    let other: Result<(), _> = retry_probe_timeouts(
        &NO_WAIT,
        || {
            calls.set(calls.get() + 1);
            async {
                Err(MicrosandboxError::Runtime(
                    "upgrade required: old msb".into(),
                ))
            }
        },
        yes,
    )
    .await;
    assert!(!is_probe_timeout(&other.unwrap_err()));
    assert_eq!(calls.get(), 1);
    let touched: Result<(), _> = retry_probe_timeouts(
        &NO_WAIT,
        || {
            calls.set(calls.get() + 1);
            async { Err(probe_timeout()) }
        },
        || async { false },
    )
    .await;
    assert!(touched.is_err());
    assert_eq!(calls.get(), 2);
}

#[test]
fn the_sdks_probe_timeout_wording_is_recognised_inside_its_upgrade_message() {
    // Wording at v0.7.12 (`launch_contract.rs`, `require_protected_guest_state`).
    assert!(is_probe_timeout(&MicrosandboxError::Runtime(
        "upgrade required: selected runtime /x/msb does not advertise protected sandbox \
             state. Probe failed: runtime error: runtime version probe timed out."
            .into()
    )));
}

#[test]
fn only_the_relay_race_counts_as_a_boot_race() {
    let boot = |message: &str| MicrosandboxError::BootStart {
        name: "a".into(),
        err: microsandbox_runtime::boot_error::BootError {
            t: "2026-10-06T00:00:00.000Z".into(),
            stage: microsandbox_runtime::boot_error::BootErrorStage::Other,
            reason: None,
            errno: None,
            message: message.into(),
        },
    };
    assert!(is_boot_race(&boot(
        "sandbox process exited (exit code: 0) before agent relay became available"
    )));
    assert!(!is_boot_race(&boot("rootfs mount failed")));
    assert!(!is_boot_race(&MicrosandboxError::Runtime(
        "before agent relay became available".into()
    )));
}

#[test]
fn log_tails_name_each_file_and_keep_the_end() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().to_path_buf();
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(dir.join("b.log"), "0123456789").unwrap();
    std::fs::write(dir.join("a.log"), "hello").unwrap();
    let tail = log_tail(&dir, 4);
    assert_eq!(tail, "--- a.log\nello\n--- b.log\n6789\n");
    assert_eq!(log_tail(&dir.join("missing"), 4), "");
}

#[test]
fn dir_names_keep_directories_and_fail_on_what_they_cannot_read() {
    use std::ffi::OsString;
    let ok = |n: &str, dir: bool| Ok((OsString::from(n), Ok(dir)));
    let names = dir_names(vec![ok("a", true), ok("file", false), ok("b", true)].into_iter());
    assert_eq!(names.unwrap().into_iter().collect::<Vec<_>>(), ["a", "b"]);

    let denied = || std::io::Error::from(std::io::ErrorKind::PermissionDenied);
    let entry_err = dir_names(vec![ok("a", true), Err(denied())].into_iter());
    assert!(
        entry_err
            .unwrap_err()
            .to_string()
            .contains("list stale dirs")
    );
    let type_err = dir_names(vec![Ok((OsString::from("a"), Err(denied())))].into_iter());
    assert!(type_err.is_err());

    // An entry removed between listing and stat is not an error.
    let vanished = || std::io::Error::from(std::io::ErrorKind::NotFound);
    let names = dir_names(
        vec![
            ok("a", true),
            Err(vanished()),
            Ok((OsString::from("b"), Err(vanished()))),
        ]
        .into_iter(),
    );
    assert_eq!(names.unwrap().into_iter().collect::<Vec<_>>(), ["a"]);
}

#[cfg(unix)]
#[test]
fn a_directory_name_that_is_not_utf8_is_listed_so_reconcile_can_call_it_foreign() {
    use std::os::unix::ffi::OsStringExt;
    let bad = std::ffi::OsString::from_vec(vec![0x66, 0xff]);
    let names = dir_names(std::iter::once(Ok((bad, Ok(true))))).unwrap();
    assert_eq!(names.into_iter().collect::<Vec<_>>(), ["f\u{fffd}"]);
}

#[tokio::test]
async fn pruning_a_sandbox_deletes_the_images_its_journal_no_longer_names() {
    let tmp = puddle_test_kit::private_tempdir();
    let base = tmp.path().to_path_buf();
    let config = MsbConfig::new(
        base.join("home"),
        base.join("no-msb"),
        base.join("no-libkrunfw"),
        base.join("s"),
    );
    let rt = MsbRuntime::open(config).await.unwrap();
    let dir = rt.sandbox_dir("ws");
    std::fs::create_dir_all(dir.join(".grow-new")).unwrap();
    std::fs::write(dir.join("upper.ext4"), b"old").unwrap();
    std::fs::write(dir.join(".grow-new").join("upper.ext4"), b"new").unwrap();
    let journal =
        serde_json::json!({ "layers": [{ "path": dir.join(".grow-new").join("upper.ext4") }] });
    std::fs::write(dir.join("root-disk.json"), journal.to_string()).unwrap();
    rt.prune_grow_orphans("ws");
    assert!(!dir.join("upper.ext4").exists());
    assert!(dir.join(".grow-new").join("upper.ext4").is_file());
}

#[tokio::test]
async fn open_gives_the_sdk_the_runtime_pair_and_home_as_arguments() {
    let tmp = puddle_test_kit::private_tempdir();
    let base = tmp.path().to_path_buf();
    let config = MsbConfig::new(
        base.join("home"),
        base.join("no-msb"),
        base.join("no-libkrunfw"),
        base.join("s"),
    );
    let rt = MsbRuntime::open(config).await.unwrap();

    let resolved = rt.inner.local.config();
    assert_eq!(resolved.home(), base.join("home"));
    assert_eq!(
        resolved.paths.msb.as_deref(),
        Some(base.join("no-msb").as_path())
    );
    assert_eq!(
        resolved.paths.libkrunfw.as_deref(),
        Some(base.join("no-libkrunfw").as_path())
    );
}

#[tokio::test]
async fn create_refuses_a_mount_outside_the_share_or_of_the_wrong_kind_before_any_vm_work() {
    use puddle_compute::{FileMount, SandboxSpec};
    use puddle_types::GuestPath;

    let tmp = puddle_test_kit::private_tempdir();
    let base = tmp.path().to_path_buf();
    let share = base.join("home").join("guest-share");
    std::fs::create_dir_all(share.join("tree")).unwrap();
    std::fs::write(share.join("file"), b"x").unwrap();
    std::fs::write(base.join("secret"), b"x").unwrap();
    // flake-ok: a path that must not exist; on Windows it is rooted on the current drive and still does not exist
    let config = MsbConfig::new(base.join("home"), "/no/msb", "/no/libkrunfw", &share);
    let rt = MsbRuntime::open(config).await.unwrap();

    let spec = |mount: FileMount| {
        SandboxSpec::new(
            SandboxName::new("mounts").unwrap(),
            ImageRef::new("debian:12").unwrap(),
        )
        .with_file_mount(mount)
    };
    let guest = || GuestPath::new("/mnt/x").unwrap();
    let refusal = |mount: FileMount| {
        let rt = &rt;
        async move { rt.create(spec(mount)).await.err().unwrap().to_string() }
    };
    let outside = refusal(FileMount::read_only_dir(&base, guest())).await;
    assert!(
        outside.contains("outside the guest-share root"),
        "{outside}"
    );
    let outside = refusal(FileMount::read_only(base.join("secret"), guest())).await;
    assert!(
        outside.contains("outside the guest-share root"),
        "{outside}"
    );
    let wrong = refusal(FileMount::read_only_dir(share.join("file"), guest())).await;
    assert!(wrong.contains("is not a directory"), "{wrong}");
    let wrong = refusal(FileMount::read_only(share.join("tree"), guest())).await;
    assert!(wrong.contains("is not a file"), "{wrong}");
    drop(rt);
}

#[tokio::test]
async fn create_refuses_a_checkout_that_breaks_its_rule_or_has_a_route_before_any_vm_work() {
    use puddle_compute::{FileMount, SandboxSpec, VsockRoute};
    use puddle_types::GuestPath;

    let tmp = puddle_test_kit::private_tempdir();
    let base = dunce::canonicalize(tmp.path()).unwrap();
    let home = base.join("data").join("msb");
    let share = base.join("data").join("guest-share");
    std::fs::create_dir_all(&share).unwrap();
    let git_dir = |at: &Path| {
        let git = at.join(".git");
        std::fs::create_dir_all(git.join("objects")).unwrap();
        std::fs::create_dir_all(git.join("refs")).unwrap();
        std::fs::write(git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        git
    };
    let checkout = git_dir(&base.join("api"));
    let in_data = git_dir(&base.join("data").join("settings"));
    let config = MsbConfig::new(&home, "/no/msb", "/no/libkrunfw", &share)
        .with_protected_dir(base.join("data"));
    let rt = MsbRuntime::open(config).await.unwrap();
    let spec = |git: &Path| {
        SandboxSpec::new(
            SandboxName::new("import").unwrap(),
            ImageRef::new("debian:12").unwrap(),
        )
        .with_file_mount(FileMount::checkout(
            git,
            GuestPath::new("/puddle/import/0").unwrap(),
        ))
    };
    let refusal = |spec: SandboxSpec| {
        let rt = &rt;
        async move { rt.create(spec).await.err().unwrap().to_string() }
    };
    let routed = refusal(spec(&checkout).with_route(VsockRoute::new(5000, "/run/p.sock"))).await;
    assert!(
        routed.contains("may only go to a sandbox with no route"),
        "{routed}"
    );
    let inside = refusal(spec(&in_data)).await;
    assert!(inside.contains("lies inside Puddle's folder"), "{inside}");
    let climbing = base.join("api").join("..").join("api").join(".git");
    let climbing = refusal(spec(&climbing)).await;
    assert!(climbing.contains("canonical path"), "{climbing}");
    drop(rt);
}

#[test]
fn the_database_folder_is_listed_with_sizes_and_ages_for_the_evidence_of_a_failed_open() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("db");
    assert!(database_files(&dir, SystemTime::now()).starts_with("cannot be listed"));
    std::fs::create_dir_all(&dir).unwrap();
    assert_eq!(database_files(&dir, SystemTime::now()), "empty");
    std::fs::write(dir.join("msb.db"), [0u8; 10]).unwrap();
    std::fs::write(dir.join("msb.db-journal"), [0u8; 3]).unwrap();
    // A file newer than the clock reading has no age to give.
    assert!(database_files(&dir, SystemTime::UNIX_EPOCH).contains("msb.db 10 B, age unknown"),);
    let later = SystemTime::now() + Duration::from_secs(60);
    let listed = database_files(&dir, later);
    assert!(
        listed.starts_with("msb.db 10 B, 6") && listed.contains("; msb.db-journal 3 B, 6"),
        "{listed}"
    );
}

#[tokio::test]
async fn a_database_that_cannot_open_says_how_long_it_took_and_what_its_folder_holds() {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().to_path_buf();
    let home = base.join("home");
    std::fs::create_dir_all(&home).unwrap();
    // A file where msb wants its database folder.
    std::fs::write(home.join("db"), b"not a folder").unwrap();
    // flake-ok: a path that must not exist; on Windows it is rooted on the current drive and still does not exist
    let config = MsbConfig::new(home, "/no/msb", "/no/libkrunfw", base.join("share"));
    let error = MsbRuntime::open(config).await.err().unwrap().to_string();
    assert!(
        error.contains("(after ")
            && error.contains("database folder")
            && error.contains("cannot be listed"),
        "{error}"
    );
}

#[test]
fn every_sdk_status_maps_one_to_one() {
    let pairs = [
        (SdkStatus::Created, WorkspaceStatus::Created),
        (SdkStatus::Starting, WorkspaceStatus::Starting),
        (SdkStatus::Running, WorkspaceStatus::Running),
        (SdkStatus::Draining, WorkspaceStatus::Draining),
        (SdkStatus::Paused, WorkspaceStatus::Paused),
        (SdkStatus::Stopped, WorkspaceStatus::Stopped),
        (SdkStatus::Crashed, WorkspaceStatus::Crashed),
    ];
    for (sdk, ours) in pairs {
        assert_eq!(status(sdk), ours);
    }
}

fn wanted(pairs: &[(&str, &str)]) -> GuestEnv {
    let mut env = GuestEnv::new();
    for (k, v) in pairs {
        env.set(k, v).unwrap();
    }
    env
}

fn stored(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

#[test]
fn the_same_variables_make_no_patch() {
    let patch = EnvPatch::between(&stored(&[("A", "1")]), &wanted(&[("A", "1")]));
    assert!(patch.is_empty());
}

#[test]
fn a_changed_added_and_dropped_variable_each_show_in_the_patch() {
    let patch = EnvPatch::between(
        &stored(&[("KEPT", "same"), ("CHANGED", "old"), ("GONE", "1")]),
        &wanted(&[("KEPT", "same"), ("CHANGED", "new"), ("ADDED", "yes")]),
    );
    assert_eq!(patch.remove, ["GONE"]);
    let mut set = patch.set;
    set.sort();
    assert_eq!(
        set,
        [
            ("ADDED".to_owned(), "yes".to_owned()),
            ("CHANGED".to_owned(), "new".to_owned())
        ]
    );
}
