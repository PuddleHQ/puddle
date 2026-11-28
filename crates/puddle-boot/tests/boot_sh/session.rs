// SPDX-License-Identifier: AGPL-3.0-or-later
//! The shell start folder, prompt name, session user and supervisor leak checks.

use super::*;

/// Starts `shell` (`interactive` or not) in `cwd` with `home` as `$HOME`, sources the start-folder
/// file `file` and prints where it ended up.
fn where_after_start_folder(
    shell: &std::path::Path,
    interactive: bool,
    cwd: &std::path::Path,
    home: &std::path::Path,
    file: &std::path::Path,
) -> String {
    let mut command = std::process::Command::new(shell);
    if interactive {
        command.arg("-i");
    }
    let out = command
        .arg("-c")
        .arg(format!(". '{}'; pwd -P", file.display()))
        .current_dir(cwd)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", home)
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

#[test]
fn the_start_folder_file_moves_only_an_interactive_shell_that_starts_in_home() {
    for sh in shells() {
        let fr = FakeRoot::new(&sh);
        let (root, api, other, home) = (
            fr.path("/workspace"),
            fr.path("/workspace/api"),
            fr.path("/elsewhere"),
            fr.path("/root-home"),
        );
        for dir in [&api, &other, &home] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let guest = |p: &std::path::Path| GuestPath::new(p.to_str().unwrap()).unwrap();
        let written = start_folder_file(&guest(&root), &guest(&api));
        assert_eq!(written.path().as_str(), START_FOLDER_FILE_GUEST);
        let file = fr.path("/start-folder.sh");
        std::fs::write(&file, written.contents()).unwrap();
        let real = |p: &std::path::Path| std::fs::canonicalize(p).unwrap().display().to_string();
        let at = |interactive: bool, cwd: &std::path::Path| {
            where_after_start_folder(&fr.shell, interactive, cwd, &home, &file)
        };
        let why = sh.display().to_string();

        // An interactive shell that starts in $HOME lands in the start folder.
        assert_eq!(at(true, &home), real(&api), "{why}");
        // A command without a terminal stays, and so does a shell started in another folder.
        assert_eq!(at(false, &home), real(&home), "{why}");
        assert_eq!(at(true, &other), real(&other), "{why}");

        // A start folder that does not exist yet opens the volume root; with no root, it stays.
        let missing = start_folder_file(&guest(&root), &guest(&fr.path("/workspace/later")));
        std::fs::write(&file, missing.contents()).unwrap();
        assert_eq!(at(true, &home), real(&root), "{why}");
        let nowhere = start_folder_file(
            &guest(&fr.path("/srv/none")),
            &guest(&fr.path("/srv/none/x")),
        );
        std::fs::write(&file, nowhere.contents()).unwrap();
        assert_eq!(at(true, &home), real(&home), "{why}");

        // The folder's name is one word to the shell, whatever it holds.
        let odd = fr.path("/workspace/it's $(touch pwned) `x`");
        std::fs::create_dir_all(&odd).unwrap();
        let quoted = start_folder_file(&guest(&root), &guest(&odd));
        std::fs::write(&file, quoted.contents()).unwrap();
        assert_eq!(at(true, &home), real(&odd), "{why}");
        assert!(!fr.path("/workspace/pwned").exists());
        assert!(!home.join("pwned").exists());
    }
}

#[test]
fn the_prompt_name_file_shows_the_workspace_name_where_bash_shows_the_host_name() {
    for sh in shells() {
        let fr = FakeRoot::new(&sh);
        let is_bash = sh.file_name().is_some_and(|n| n == "bash");
        let why = sh.display().to_string();
        let run = |name: &str, interactive: bool, script: &str| {
            let written = prompt_name_file(name);
            assert_eq!(written.path().as_str(), PROMPT_NAME_FILE_GUEST);
            let file = fr.path("/prompt-name.sh");
            std::fs::write(&file, written.contents()).unwrap();
            let mut command = std::process::Command::new(&fr.shell);
            if interactive {
                command.arg("-i");
            }
            let out = command
                .arg("-c")
                .arg(format!(". '{}'; {script}", file.display()))
                .current_dir(fr.path("/"))
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("HOME", fr.path("/"))
                .output()
                .unwrap();
            assert!(out.status.success(), "{why}: {out:?}");
            String::from_utf8(out.stdout).unwrap().trim().to_owned()
        };
        // Every shell can source it, interactive or not, and none of them runs anything.
        assert_eq!(run("qa-first", false, "echo ok"), "ok", "{why}");
        assert_eq!(run("qa-first", true, "echo ok"), "ok", "{why}");
        if !is_bash {
            continue;
        }
        let shown = |name: &str| {
            run(
                name,
                true,
                "PS1='agent@\\h:\\H$ '; eval \"$PROMPT_COMMAND\"; echo \"${PS1@P}\"",
            )
        };
        // The host name is swapped for the name, in both spellings of it.
        assert_eq!(shown("qa-first"), "agent@qa-first:qa-first$", "{why}");
        // Whatever the name holds is text: nothing runs and nothing expands.
        let odd = "it's $(touch pwned) `x` $HOME \\h \\u";
        assert_eq!(shown(odd), format!("agent@{odd}:{odd}$"), "{why}");
        assert!(!fr.path("/pwned").exists());
        // A non-interactive bash keeps its variables to itself.
        assert_eq!(
            run("qa-first", false, "echo \"[${PROMPT_COMMAND-}]\""),
            "[]",
            "{why}"
        );
    }
}

/// An image's user database: root, and `vscode` at uid 1000 as in the default devcontainer image.
fn user_database(fr: &FakeRoot, docker_group: bool) {
    std::fs::write(
        fr.path("/etc/passwd"),
        "root:x:0:0:root:/root:/bin/bash\nvscode:x:1000:1000::/home/vscode:/bin/bash\n",
    )
    .unwrap();
    let docker = if docker_group {
        "docker:x:998:vscode\n"
    } else {
        ""
    };
    std::fs::write(
        fr.path("/etc/group"),
        format!("root:x:0:\nvscode:x:1000:\n{docker}"),
    )
    .unwrap();
    std::fs::write(fr.path("/etc/shadow"), "root:*:19000:0:99999:7:::\n").unwrap();
}

fn agent_plan(path: &str) -> BootPlan {
    let spec = MergeSpec::new(
        MergeFormat::Json,
        vec![MergeEntry::json(&["proxies"], &serde_json::json!({}))],
    )
    .unwrap();
    BootPlan::builder(&image(&[], &["bash"]))
        .session_user(SessionUser::Agent)
        .file(file(path, b"x\n"))
        .file(GuestFile::merged(
            GuestPath::new("/home/agent/.docker/config.json").unwrap(),
            spec,
        ))
        .no_agent()
        .build()
        .unwrap()
}

#[test]
fn the_hook_adds_the_session_user_at_the_first_free_uid_and_keeps_it_after() {
    for sh in shells() {
        let fr = FakeRoot::new(&sh);
        user_database(&fr, true);
        let plan = agent_plan("/home/agent/.gradle/init.d/puddle-proxy.gradle");
        let out = fr.run(&plan);
        assert!(out.status.success(), "{}: {}", sh.display(), show(&out));
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("session user agent added (uid 1001)"),
            "{stdout}"
        );
        assert!(stdout.contains("no sudo in this image"), "{stdout}");
        assert!(
            stdout
                .lines()
                .any(|l| l == "puddle-boot: session user cannot become root: no sudo"),
            "{stdout}"
        );

        let user_rows = fr.read("/etc/passwd");
        assert!(
            user_rows.ends_with("agent:x:1001:1001::/home/agent:/bin/sh\n"),
            "the agent row is not the last line of the user database (uid 1001)"
        );
        let group = fr.read("/etc/group");
        assert!(group.contains("\nagent:x:1001:\n"), "{group}");
        assert!(group.contains("\ndocker:x:998:vscode,agent\n"), "{group}");
        assert!(fr.read("/etc/shadow").contains("\nagent:*:"));
        assert_eq!(
            fr.read(SUDOERS_SESSION_GUEST).lines().last(),
            Some("agent ALL=(ALL:ALL) NOPASSWD: ALL")
        );
        let mode = std::fs::metadata(fr.path(SUDOERS_SESSION_GUEST))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o440);

        // Everything made below the home is the user's: the home, each new directory, each file.
        let owned = fr.owned();
        for path in [
            "/home/agent",
            "/home/agent/.gradle",
            "/home/agent/.gradle/init.d",
            "/home/agent/.gradle/init.d/puddle-proxy.gradle",
            "/home/agent/.docker",
            "/home/agent/.vscode-server/data/Machine",
        ] {
            assert!(
                owned.contains(&format!("1001:1001 {path}")),
                "{path}: {owned:?}"
            );
        }
        assert!(
            owned.iter().all(|l| l.starts_with("1001:1001 /home/agent")),
            "{owned:?}"
        );
        // The Machine settings are below the agent's home, not root's.
        assert!(!fr.path(MACHINE_SETTINGS_GUEST).exists());

        // A second boot finds the user and adds nothing twice. A merged file that exists (the
        // stub merge tool writes none) is the user's too.
        let machine = machine_settings_path(SessionUser::Agent);
        std::fs::write(fr.path(&machine), "{}").unwrap();
        let out = fr.run(&plan);
        assert!(out.status.success(), "{}", show(&out));
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(!stdout.contains("added"), "{stdout}");
        assert!(stdout.contains("session user agent (uid 1001)"), "{stdout}");
        assert_eq!(fr.read("/etc/passwd"), user_rows);
        assert_eq!(fr.read("/etc/group"), group);
        assert!(
            fr.owned().contains(&format!("1001:1001 {machine}")),
            "{:?}",
            fr.owned()
        );
    }
}

#[test]
fn per_user_files_an_image_variable_moved_out_of_the_home_still_belong_to_the_session_user() {
    for sh in shells() {
        let fr = FakeRoot::new(&sh);
        user_database(&fr, true);
        std::fs::create_dir_all(fr.path("/opt")).unwrap();
        let spec = MergeSpec::new(
            MergeFormat::Json,
            vec![MergeEntry::json(&["proxies"], &serde_json::json!({}))],
        )
        .unwrap();
        let plan = BootPlan::builder(&image(&[], &["bash"]))
            .session_user(SessionUser::Agent)
            .file(file("/opt/gradle/init.d/a.gradle", b"x\n").with_session_owner())
            .file(
                GuestFile::merged(GuestPath::new("/opt/docker/cli/config.json").unwrap(), spec)
                    .with_session_owner(),
            )
            .file(file("/opt/other/b", b"y\n"))
            .no_agent()
            .build()
            .unwrap();
        let out = fr.run(&plan);
        assert!(out.status.success(), "{}: {}", sh.display(), show(&out));
        let owned = fr.owned();
        for path in [
            "/opt/gradle",
            "/opt/gradle/init.d",
            "/opt/gradle/init.d/a.gradle",
            "/opt/docker",
            "/opt/docker/cli",
        ] {
            assert!(
                owned.contains(&format!("1001:1001 {path}")),
                "{path}: {owned:?}"
            );
        }
        // The directory that was there stays root's, and so does a file nobody gave away.
        assert!(!owned.iter().any(|l| l.ends_with(" /opt")), "{owned:?}");
        assert!(!owned.iter().any(|l| l.contains("/opt/other")), "{owned:?}");

        // A merged file that exists is the user's on the next boot.
        std::fs::write(fr.path("/opt/docker/cli/config.json"), "{}").unwrap();
        let before = fr.owned().len();
        let out = fr.run(&plan);
        assert!(out.status.success(), "{}", show(&out));
        assert!(
            fr.owned()[before..].contains(&"1001:1001 /opt/docker/cli/config.json".to_owned()),
            "{:?}",
            fr.owned()
        );
    }
}

#[test]
fn the_session_user_gets_uid_1000_when_it_is_free_and_bash_and_sudo_when_the_image_has_them() {
    let fr = FakeRoot::new(&shells()[0]);
    std::fs::write(fr.path("/etc/passwd"), "root:x:0:0:root:/root:/bin/sh\n").unwrap();
    std::fs::write(fr.path("/etc/group"), "root:x:0:\n").unwrap();
    for exe in ["/bin/bash", "/usr/bin/sudo"] {
        std::fs::create_dir_all(fr.path(exe).parent().unwrap()).unwrap();
        puddle_test_kit::write_executable(&fr.path(exe), "");
    }
    let out = fr.run(&agent_plan("/etc/puddle-test"));
    assert!(out.status.success(), "{}", show(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("session user agent (uid 1000), sudo at /usr/bin/sudo"),
        "{stdout}"
    );
    assert!(!stdout.contains("cannot become root"), "{stdout}");
    assert!(
        fr.read("/etc/passwd")
            .ends_with("agent:x:1000:1000::/home/agent:/bin/bash\n")
    );
    // No shadow file in this image, and none made; a file outside the home stays root's.
    assert!(!fr.path("/etc/shadow").exists());
    assert!(!fr.owned().iter().any(|l| l.ends_with("/etc/puddle-test")));
}

#[test]
fn a_root_session_user_changes_no_user_database_and_owns_nothing() {
    let fr = FakeRoot::new(&shells()[0]);
    user_database(&fr, true);
    let plan = BootPlan::builder(&image(&[], &["bash"]))
        .file(file("/root/.gradle/init.d/puddle-proxy.gradle", b"x\n"))
        .no_agent()
        .build()
        .unwrap();
    assert_eq!(plan.session_user(), SessionUser::Root);
    let out = fr.run(&plan);
    assert!(out.status.success(), "{}", show(&out));
    assert!(!fr.read("/etc/passwd").contains("agent"));
    assert!(fr.owned().is_empty(), "{:?}", fr.owned());
    assert!(!fr.path(SUDOERS_SESSION_GUEST).exists());
    assert!(
        !String::from_utf8(plan.render())
            .unwrap()
            .contains("puddle_session_user")
    );
}

const LEAK_ENV: &str = "PUDDLE_BOOT_TEST_LEAK_REPORT";

/// Not a test of its own: the child process of `a_killed_test_process_leaves_no_supervisor_or_folder`,
/// which runs it with `--ignored`. Starts the hook, reports where, and waits to be killed.
#[test]
#[ignore = "child process of a_killed_test_process_leaves_no_supervisor_or_folder"]
fn killed_process_helper() {
    let report = std::env::var_os(LEAK_ENV).expect("set by the parent test");
    let fr = FakeRoot::new(&shells()[0]);
    assert!(fr.run(&full_plan("CA\n")).status.success());
    let line = format!(
        "{} {}",
        fr.recorded_pid("supervisor.pid").unwrap(),
        fr.root.display()
    );
    std::fs::write(&report, line).unwrap();
    loop {
        std::thread::park();
    }
}

/// nextest SIGTERMs a test that outlives its timeout and never runs `FakeRoot::drop`; the
/// supervisor and the folder under `/tmp` used to stay for good (14 supervisors, 4.5 h).
#[test]
fn a_killed_test_process_leaves_no_supervisor_or_folder() {
    let report = tempfile::NamedTempFile::new().unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "session::killed_process_helper"])
        .env(LEAK_ENV, report.path())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let line = eventually_blocking(
        "the helper reports its root",
        Duration::from_secs(30),
        || {
            Some(std::fs::read_to_string(report.path()).unwrap_or_default())
                .filter(|l| !l.is_empty())
        },
    );
    let (supervisor, root) = line.split_once(' ').unwrap();
    let supervisor: u32 = supervisor.parse().unwrap();
    let root = std::path::PathBuf::from(root);
    assert!(alive(supervisor) && root.exists());
    child.kill().unwrap();
    child.wait().unwrap();
    eventually_blocking(
        "the supervisor is gone and its folder removed",
        Duration::from_secs(20),
        || !alive(supervisor) && !root.exists(),
    );
}
