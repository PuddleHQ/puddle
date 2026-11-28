// SPDX-License-Identifier: AGPL-3.0-or-later
//! Boot applies the plan: steps, convergence, files and their ownership.

use super::*;

#[test]
fn boot_applies_every_step() {
    for sh in shells() {
        let fr = FakeRoot::new(&sh);
        let out = fr.run(&full_plan("CA-1\n"));
        assert!(out.status.success(), "{}: {}", sh.display(), show(&out));
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.ends_with("puddle-boot: ready\n"), "{stdout}");

        // Kernel settings, through sysctl, read back.
        assert_eq!(fr.read("/proc/sys/fs/inotify/max_user_instances"), "1024\n");
        assert_eq!(fr.read("/proc/sys/fs/inotify/max_user_watches"), "524288\n");
        assert_eq!(fr.read("/proc/sys/kernel/unprivileged_bpf_disabled"), "1\n");
        assert_eq!(
            fr.read("/proc/sys/net/ipv4/ip_unprivileged_port_start"),
            "0\n"
        );
        let calls = fr.calls();
        assert_eq!(count(&calls, "sysctl -w"), 4, "{calls:?}");

        // Files: contents and modes.
        assert_eq!(fr.read(CA), "CA-1\n");
        assert_eq!(fr.read("/etc/npmrc"), "proxy=http://127.0.0.1:3128\n");
        let mode = |p: &str| std::fs::metadata(fr.path(p)).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode("/etc/npmrc"), 0o600);
        assert_eq!(mode(ENV_FILE_GUEST), 0o644);
        // VS Code's Machine settings go through the merge tool as JSONC.
        let machine = fr.read(&format!(
            "/merge-specs/{}",
            MACHINE_SETTINGS_GUEST.replace('/', "_")
        ));
        assert!(
            machine.contains("\"jsonc\"") && machine.contains("process"),
            "{machine}"
        );

        // Login shells: the image PATH back, the env with odd values intact.
        assert_eq!(
            source_and_print(
                &fr,
                &["/etc/profile.d/01-puddle-env.sh", PATH_FILE_GUEST],
                "$PATH"
            ),
            IMAGE_PATH
        );
        assert_eq!(
            source_and_print(&fr, &[ENV_FILE_GUEST], "$ODD"),
            "it's $HOME `x` \\ \"q\"\nline2"
        );

        // git: one include, fsync and identity in Puddle's file.
        assert_eq!(
            fr.read("/etc/gitconfig"),
            format!("[include]\n\tpath = {GIT_CONFIG_GUEST}\n")
        );
        let git = fr.read(GIT_CONFIG_GUEST);
        assert!(
            git.contains("fsync = added")
                && git.contains("fsyncMethod = batch")
                && git.contains("name = \"Ada Lovelace\"")
        );

        // CA changed (first boot): update-ca-certificates ran once.
        assert_eq!(count(&calls, "update-ca-certificates"), 1);

        // Agent up under its supervisor; ready marker written.
        let agent = fr.agent_pids();
        assert_eq!(agent.len(), 1);
        assert!(alive(agent[0]));
        assert!(alive(fr.recorded_pid("supervisor.pid").unwrap()));
        assert_eq!(fr.read("/run/puddle/boot.done"), "boot-1\n");
        // No ENTRYPOINT declared (devcontainers image): nothing chained.
        assert!(fr.recorded_pid("entrypoint.pid").is_none());
    }
}

#[test]
fn second_boot_converges_and_skips_an_unchanged_ca() {
    for sh in shells() {
        let fr = FakeRoot::new(&sh);
        assert!(fr.run(&full_plan("CA-1\n")).status.success());
        let supervisor = fr.recorded_pid("supervisor.pid").unwrap();
        let out = fr.run(&full_plan("CA-1\n"));
        assert!(out.status.success(), "{}", show(&out));
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("update-ca-certificates skipped"),
            "{stdout}"
        );
        assert!(
            stdout.contains("agent supervisor already running"),
            "{stdout}"
        );
        assert_eq!(count(&fr.calls(), "update-ca-certificates"), 1);
        assert_eq!(fr.recorded_pid("supervisor.pid"), Some(supervisor));
        assert_eq!(fr.agent_pids().len(), 1, "agent started twice");
        assert_eq!(fr.read("/etc/gitconfig").matches("[include]").count(), 1);

        // A changed root: the step runs again.
        assert!(fr.run(&full_plan("CA-2\n")).status.success());
        assert_eq!(count(&fr.calls(), "update-ca-certificates"), 2);
    }
}

#[test]
fn after_a_restart_the_hook_redoes_the_sysctls_and_restarts_the_agent() {
    let fr = FakeRoot::new(&shells()[0]);
    assert!(fr.run(&full_plan("CA\n")).status.success());
    let old_supervisor = fr.recorded_pid("supervisor.pid").unwrap();
    // A reboot: new boot id, kernel settings back at defaults, processes gone.
    kill9(old_supervisor);
    for pid in fr.agent_pids() {
        kill9(pid);
    }
    std::fs::write(fr.path("/proc/sys/kernel/random/boot_id"), "boot-2\n").unwrap();
    std::fs::write(fr.path("/proc/sys/kernel/unprivileged_bpf_disabled"), "0\n").unwrap();
    std::fs::write(fr.path("/proc/net/tcp"), "  sl  local_address\n").unwrap();
    let out = fr.run(&full_plan("CA\n"));
    assert!(out.status.success(), "{}", show(&out));
    assert_eq!(fr.read("/proc/sys/kernel/unprivileged_bpf_disabled"), "1\n");
    assert_ne!(fr.recorded_pid("supervisor.pid"), Some(old_supervisor));
    assert_eq!(fr.agent_pids().len(), 2);
    assert_eq!(fr.read("/run/puddle/boot.done"), "boot-2\n");
}

#[test]
fn files_dropped_from_the_plan_are_removed_and_trigger_the_ca_step() {
    let fr = FakeRoot::new(&shells()[0]);
    assert!(fr.run(&full_plan("CA\n")).status.success());
    let without = BootPlan::builder(&image(&[], &["bash"])).build().unwrap();
    let out = fr.run(&without);
    assert!(out.status.success(), "{}", show(&out));
    assert!(!fr.path(CA).exists());
    assert!(!fr.path("/etc/npmrc").exists());
    assert_eq!(count(&fr.calls(), "update-ca-certificates"), 2);
    assert!(fr.path(ENV_FILE_GUEST).exists());
}

/// A workspace made by an older Puddle holds the old git settings; its next boot rewrites them.
#[test]
fn an_existing_workspace_gets_the_current_git_fsync_settings_at_its_next_boot() {
    let fr = FakeRoot::new(&shells()[0]);
    let first = fr.run(&full_plan("CA\n"));
    assert!(first.status.success(), "{}", show(&first));
    std::fs::write(
        fr.path(GIT_CONFIG_GUEST),
        "[core]\n\tfsync = committed\n[user]\n\tname = \"Ada Lovelace\"\n",
    )
    .unwrap();
    let second = fr.run(&full_plan("CA\n"));
    assert!(second.status.success(), "{}", show(&second));
    let git = fr.read(GIT_CONFIG_GUEST);
    assert!(
        git.contains("fsync = added\n") && git.contains("fsyncMethod = batch\n"),
        "{git}"
    );
    assert!(!git.contains("committed"), "{git}");
}

#[test]
fn a_file_puddle_owns_that_was_changed_in_the_guest_is_reported_as_replaced() {
    let fr = FakeRoot::new(&shells()[0]);
    let first = fr.run(&full_plan("CA\n"));
    assert!(first.status.success(), "{}", show(&first));
    // New files are not "replaced", and neither is the second boot of an untouched one.
    let stdout = |out: &std::process::Output| String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(!stdout(&first).contains("puddle-boot: replaced"));
    let second = fr.run(&full_plan("CA\n"));
    assert!(
        !stdout(&second).contains("puddle-boot: replaced"),
        "{}",
        stdout(&second)
    );

    // The workspace edits one of Puddle's files, and puts a link where another was.
    std::fs::write(fr.path("/etc/npmrc"), "registry=https://evil.example\n").unwrap();
    std::fs::remove_file(fr.path(CA)).unwrap();
    std::os::unix::fs::symlink("/etc/passwd", fr.path(CA)).unwrap();
    let third = fr.run(&full_plan("CA\n"));
    assert!(third.status.success(), "{}", show(&third));
    let said = stdout(&third);
    assert!(
        said.contains("puddle-boot: replaced /etc/npmrc\n"),
        "{said}"
    );
    assert!(
        said.contains(&format!("puddle-boot: replaced {CA}\n")),
        "{said}"
    );
    // Put back, and the host's text is what the guest holds.
    assert_eq!(fr.read("/etc/npmrc"), "proxy=http://127.0.0.1:3128\n");
    assert!(!fr.path(CA).is_symlink());
}

#[test]
fn every_byte_survives_the_plan() {
    let fr = FakeRoot::new(&shells()[0]);
    let all: Vec<u8> = (0..=255).cycle().take(4096).collect();
    let plan = BootPlan::builder(&ImageConfig::default())
        .no_agent()
        .file(file("/opt/blob.bin", &all))
        .file(file("/opt/with space/it's", b"-n %s\\"))
        .build()
        .unwrap();
    let out = fr.run(&plan);
    assert!(out.status.success(), "{}", show(&out));
    assert_eq!(std::fs::read(fr.path("/opt/blob.bin")).unwrap(), all);
    assert_eq!(
        std::fs::read(fr.path("/opt/with space/it's")).unwrap(),
        b"-n %s\\"
    );
}

#[test]
fn a_symlink_planted_at_a_file_path_is_replaced_not_followed() {
    let fr = FakeRoot::new(&shells()[0]);
    std::fs::create_dir_all(fr.path("/etc/profile.d")).unwrap();
    std::fs::write(fr.path("/etc/victim"), "keep").unwrap();
    std::os::unix::fs::symlink(fr.path("/etc/victim"), fr.path(ENV_FILE_GUEST)).unwrap();
    let plan = BootPlan::builder(&ImageConfig::default())
        .no_agent()
        .build()
        .unwrap();
    assert!(fr.run(&plan).status.success());
    assert_eq!(fr.read("/etc/victim"), "keep");
    assert!(!fr.path(ENV_FILE_GUEST).is_symlink());
}
