// SPDX-License-Identifier: GPL-3.0-or-later
//! The real `guest/boot.sh`, run on the test host against a fake root with stubbed `sysctl`,
//! `update-ca-certificates` and `puddle-agent`, under every POSIX shell the host has (dash and
//! bash at least). The VM version of these checks is `tests/vm_boot.rs`.
#![cfg(unix)]
#![expect(
    clippy::unwrap_used,
    reason = "test helpers outside #[test] fns: a failed setup fails the test"
)]

mod common;

use std::os::unix::fs::PermissionsExt as _;
use std::time::Duration;

use common::{FakeRoot, alive, kill9, shells, show};
use puddle_boot::{
    BootHook, BootPlan, CA_DIR_GUEST, ENV_FILE_GUEST, GIT_CONFIG_GUEST, Gate, GateState,
    GitIdentity, MACHINE_SETTINGS_GUEST, PATH_FILE_GUEST, PLAN_HEADER, START_FOLDER_FILE_GUEST,
    start_folder_file,
};
use puddle_compute::ImageConfig;
use puddle_test_kit::eventually_blocking;
use puddle_types::{GuestEnv, GuestFile, GuestPath, MergeEntry, MergeFormat, MergeSpec};

const IMAGE_PATH: &str = "/usr/local/cargo/bin:/usr/local/bin:/usr/bin:/bin";

fn image(entrypoint: &[String], cmd: &[&str]) -> ImageConfig {
    ImageConfig {
        entrypoint: entrypoint.to_vec(),
        cmd: cmd.iter().map(|s| (*s).to_owned()).collect(),
        env: vec![("PATH".into(), IMAGE_PATH.into())],
        ..ImageConfig::default()
    }
}

fn env() -> GuestEnv {
    let mut env = GuestEnv::new();
    env.set("HTTPS_PROXY", "http://127.0.0.1:3128").unwrap();
    env.set("ODD", "it's $HOME `x` \\ \"q\"\nline2").unwrap();
    env
}

fn file(path: &str, contents: &[u8]) -> GuestFile {
    GuestFile::new(GuestPath::new(path).unwrap(), contents.to_vec())
}

const CA: &str = "/usr/local/share/ca-certificates/puddle/root.crt";

fn full_plan(ca: &str) -> BootPlan {
    BootPlan::builder(&image(&[], &["bash"]))
        .env(&env())
        .file(file(CA, ca.as_bytes()))
        .file(
            file("/etc/npmrc", b"proxy=http://127.0.0.1:3128\n")
                .with_mode(0o600)
                .unwrap(),
        )
        .git_identity(GitIdentity::new("Ada Lovelace", "ada@example.org").unwrap())
        .build()
        .unwrap()
}

fn count(calls: &[String], prefix: &str) -> usize {
    calls.iter().filter(|c| c.starts_with(prefix)).count()
}

/// Sources `files` in a fresh shell and prints `expr`.
fn source_and_print(fr: &FakeRoot, files: &[&str], expr: &str) -> String {
    let mut parts: Vec<String> = files
        .iter()
        .map(|f| format!(". '{}';", fr.path(f).display()))
        .collect();
    parts.push(format!("printf '%s' \"{expr}\""));
    let script = parts.join(" ");
    let out = std::process::Command::new(&fr.shell)
        .arg("-c")
        .arg(script)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap()
}

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
        let calls = fr.calls();
        assert_eq!(count(&calls, "sysctl -w"), 3, "{calls:?}");

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
        assert!(git.contains("fsync = committed") && git.contains("name = \"Ada Lovelace\""));

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

#[test]
fn agent_is_back_within_a_second_after_kill_9() {
    let fr = FakeRoot::new(&shells()[0]);
    assert!(fr.run(&full_plan("CA\n")).status.success());
    let first = fr.agent_pids()[0];
    kill9(first);
    eventually_blocking(
        "the supervisor restarts the agent",
        Duration::from_secs(1),
        || {
            let pids = fr.agent_pids();
            if pids.len() == 2 && alive(pids[1]) {
                Ok(())
            } else {
                Err(format!("pids {pids:?}"))
            }
        },
    );
}

#[test]
fn the_agent_starts_with_the_plans_settings_and_a_restarted_one_has_the_latest() {
    let fr = FakeRoot::new(&shells()[0]);
    let plan = |value: &str| {
        BootPlan::builder(&ImageConfig::default())
            .agent_env("PUDDLE_AGENT_TEST", value)
            .build()
            .unwrap()
    };
    // Spaces, quotes, `$` and a backslash reach the agent as they are.
    let odd = "a b 'q' \"d\" $HOME \\ end";
    assert!(fr.run(&plan(odd)).status.success());
    assert_eq!(fr.agent_saw(), [odd]);
    // A second boot while the supervisor runs changes what the next start sees.
    assert!(fr.run(&plan("second")).status.success());
    kill9(fr.agent_pids()[0]);
    eventually_blocking(
        "the agent restarts with the new environment",
        Duration::from_secs(3),
        || {
            let saw = fr.agent_saw();
            if saw.len() == 2 {
                Ok(())
            } else {
                Err(format!("starts {saw:?}"))
            }
        },
    );
    assert_eq!(fr.agent_saw(), [odd, "second"]);
    assert!(!fr.path("/run/puddle/agent.env.new").exists());
    // A plan without settings clears them.
    assert!(
        fr.run(&BootPlan::builder(&ImageConfig::default()).build().unwrap())
            .status
            .success()
    );
    kill9(fr.agent_pids()[1]);
    eventually_blocking(
        "the agent restarts a third time",
        Duration::from_secs(3),
        || {
            let saw = fr.agent_saw();
            if saw.len() == 3 {
                Ok(())
            } else {
                Err(format!("starts {saw:?}"))
            }
        },
    );
    assert_eq!(fr.agent_saw().last().map(String::as_str), Some("unset"));
}

#[test]
fn entrypoint_is_chained_once_with_puddle_env_and_workdir() {
    for sh in shells() {
        let fr = FakeRoot::new(&sh);
        let entry = fr.path("/entry.sh");
        std::fs::write(
            &entry,
            "#!/bin/sh\nprintf '%s|%s|%s\\n' \"$HTTPS_PROXY\" \"$*\" \"$(pwd)\" >>\"$PUDDLE_ROOT/entry.out\"\nexec sleep 1000\n",
        )
        .unwrap();
        std::fs::set_permissions(&entry, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::create_dir_all(fr.path("/srv")).unwrap();
        let mut img = image(&[entry.display().to_string()], &["--flag"]);
        img.working_dir = Some("/srv".into());
        let plan = BootPlan::builder(&img).env(&env()).build().unwrap();
        // The supervisor's restarts don't matter here; the entrypoint's own PID does.
        let out = fr.run(&plan);
        assert!(out.status.success(), "{}", show(&out));
        let pid = fr.recorded_pid("entrypoint.pid").unwrap();
        eventually_blocking(
            "the entrypoint writes its output",
            Duration::from_secs(5),
            || fr.path("/entry.out").exists(),
        );
        assert_eq!(
            fr.read("/entry.out"),
            format!(
                "http://127.0.0.1:3128|--flag|{}\n",
                fr.path("/srv").display()
            )
        );
        let out = fr.run(&plan);
        assert!(String::from_utf8_lossy(&out.stdout).contains("entrypoint already running"));
        assert_eq!(fr.recorded_pid("entrypoint.pid"), Some(pid));
        assert!(alive(pid));
    }
}

/// Boots `script` as the image's entrypoint and returns what the hook said.
fn entrypoint_report(sh: &std::path::Path, script: &str) -> String {
    let fr = FakeRoot::new(sh);
    let entry = fr.path("/entry.sh");
    std::fs::write(&entry, script).unwrap();
    std::fs::set_permissions(&entry, std::fs::Permissions::from_mode(0o755)).unwrap();
    let plan = BootPlan::builder(&image(&[entry.display().to_string()], &[]))
        .build()
        .unwrap();
    let out = fr.run(&plan);
    assert!(out.status.success(), "{}", show(&out));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn an_entrypoint_that_fails_at_once_is_reported_with_its_status_and_output() {
    for sh in shells() {
        let said = entrypoint_report(
            &sh,
            "#!/bin/sh\necho 'cannot open /dev/kvm' >&2\necho second\nexit 3\n",
        );
        let line = said
            .lines()
            .find(|l| l.contains("entrypoint exited"))
            .unwrap_or_else(|| panic!("no report in:\n{said}"));
        assert_eq!(
            line,
            "puddle-boot: entrypoint exited with status 3: cannot open /dev/kvm second"
        );
        assert!(
            said.contains("puddle-boot: ready"),
            "the boot still ends ready:\n{said}"
        );
    }
}

#[test]
fn an_entrypoint_that_cannot_start_says_so() {
    for sh in shells() {
        let said = entrypoint_report(&sh, "#!/nonexistent/interpreter\n");
        assert!(
            said.contains("puddle-boot: entrypoint exited with status 127:"),
            "{said}"
        );
    }
}

/// A command that takes `secs` before it runs, standing for a process spawn on a host with no CPU to spare.
const SLOW_STUB: &str = "#!/bin/sh\nPATH=/usr/bin:/bin\nsleep SECS\nexec NAME \"$@\"\n";

fn slow(name: &str, secs: &str) -> String {
    SLOW_STUB.replace("SECS", secs).replace("NAME", name)
}

#[test]
fn a_failure_at_once_is_reported_however_slow_the_host_runs_the_wrapper() {
    for ((stub, secs), sh) in [("tail", "2"), ("setsid", "2")]
        .into_iter()
        .flat_map(|c| shells().into_iter().map(move |sh| (c, sh)))
    {
        let fr = FakeRoot::new(&sh);
        fr.stub(stub, &slow(stub, secs));
        let entry = fr.path("/entry.sh");
        std::fs::write(&entry, "#!/nonexistent/interpreter\n").unwrap();
        std::fs::set_permissions(&entry, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut img = image(&[entry.display().to_string()], &[]);
        // The wrapper reads its commands from the image's PATH; the hook's own from the stubs.
        img.env = vec![(
            "PATH".into(),
            format!("{}:{IMAGE_PATH}", fr.path("/stubs").display()),
        )];
        let plan = BootPlan::builder(&img).build().unwrap();
        let said = String::from_utf8_lossy(&fr.run(&plan).stdout).into_owned();
        assert!(
            said.contains("puddle-boot: entrypoint exited with status 127:"),
            "slow {stub}: {said}"
        );
    }
}

#[test]
fn an_entrypoint_that_finishes_cleanly_or_keeps_running_is_not_reported() {
    for sh in shells() {
        for script in ["#!/bin/sh\nexit 0\n", "#!/bin/sh\nexec sleep 1000\n"] {
            let said = entrypoint_report(&sh, script);
            assert!(!said.contains("entrypoint exited"), "{script}: {said}");
        }
    }
}

/// The exit line the wrapper writes for the guest agent, once `script` has ended.
fn exit_line(sh: &std::path::Path, script: &str) -> (String, String) {
    let fr = FakeRoot::new(sh);
    let entry = fr.path("/entry.sh");
    std::fs::write(&entry, script).unwrap();
    std::fs::set_permissions(&entry, std::fs::Permissions::from_mode(0o755)).unwrap();
    let plan = BootPlan::builder(&image(&[entry.display().to_string()], &[]))
        .build()
        .unwrap();
    let out = fr.run(&plan);
    assert!(out.status.success(), "{}", show(&out));
    eventually_blocking(
        "the entrypoint's exit line",
        Duration::from_secs(10),
        || {
            if fr.path("/run/puddle/entrypoint.exit").exists() {
                Ok(())
            } else {
                Err(format!("boot output:\n{}", show(&out)))
            }
        },
    );
    let boot_id = fr.read("/proc/sys/kernel/random/boot_id").trim().to_owned();
    (fr.read("/run/puddle/entrypoint.exit"), boot_id)
}

#[test]
fn an_entrypoint_that_fails_after_the_hook_left_leaves_its_exit_for_the_agent() {
    for sh in shells() {
        let (line, boot) = exit_line(
            &sh,
            "#!/bin/sh\nsleep 2\necho 'dockerd: cannot open\tdevice' >&2\nexit 5\n",
        );
        assert_eq!(
            line,
            format!("{boot} 5 dockerd: cannot open device\n"),
            "{sh:?}"
        );
    }
}

#[test]
fn an_entrypoint_that_fails_at_once_or_ends_cleanly_leaves_its_exit_too() {
    for sh in shells() {
        let (line, boot) = exit_line(&sh, "#!/bin/sh\necho bye\nexit 0\n");
        assert_eq!(line, format!("{boot} 0 bye\n"), "{sh:?}");
        let (line, boot) = exit_line(&sh, "#!/bin/sh\nexit 3\n");
        assert_eq!(line, format!("{boot} 3 \n"), "{sh:?}");
    }
}

#[test]
fn boot_d_steps_run_in_name_order_and_a_failing_one_fails_the_boot() {
    let fr = FakeRoot::new(&shells()[0]);
    std::fs::create_dir_all(fr.path("/puddle/boot.d")).unwrap();
    for (name, n) in [("20-b.sh", 2), ("10-a.sh", 1)] {
        std::fs::write(
            fr.path(&format!("/puddle/boot.d/{name}")),
            format!("echo step{n} >>\"$PUDDLE_ROOT/steps\"\n"),
        )
        .unwrap();
    }
    let plan = BootPlan::builder(&ImageConfig::default())
        .no_agent()
        .build()
        .unwrap();
    assert!(fr.run(&plan).status.success());
    assert_eq!(fr.read("/steps"), "step1\nstep2\n");
    std::fs::write(
        fr.path("/puddle/boot.d/30-bad.sh"),
        "echo nope >&2; exit 4\n",
    )
    .unwrap();
    let out = fr.run(&plan);
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("step 30-bad.sh failed: nope"), "{stderr}");
    assert!(!fr.path("/run/puddle/boot.done").exists());
}

#[test]
fn failures_exit_non_zero_with_the_reason_on_stderr() {
    let fr = FakeRoot::new(&shells()[0]);
    let plan = full_plan("CA\n");
    let rendered = plan.render();
    let err = |out: &std::process::Output| String::from_utf8_lossy(&out.stderr).into_owned();

    let truncated = &rendered[..rendered.len() / 2];
    let out = fr.run_raw(truncated, &[], true);
    assert_eq!(out.status.code(), Some(2), "{}", show(&out));
    assert!(err(&out).contains("truncated"));

    let out = fr.run_raw(b"# something else\npuddle_plan_end\n", &[], true);
    assert_eq!(out.status.code(), Some(2));
    assert!(err(&out).contains(PLAN_HEADER));

    std::fs::write(fr.path("/fail-update-ca"), "").unwrap();
    let out = fr.run(&plan);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        err(&out).starts_with(
            "puddle-boot: error: update-ca-certificates failed: boom: bad certificate"
        ),
        "{}",
        err(&out)
    );
    assert!(!fr.path("/run/puddle/boot.done").exists());
}

#[test]
fn a_missing_or_silent_agent_fails_the_boot() {
    let fr = FakeRoot::new(&shells()[0]);
    let missing = BootPlan::builder(&ImageConfig::default())
        .agent(puddle_boot::AgentConfig {
            binary: GuestPath::new("/puddle/none").unwrap(),
            port: 3128,
        })
        .build()
        .unwrap();
    let out = fr.run(&missing);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("/puddle/none is missing"));

    std::fs::write(
        fr.path("/puddle/mute"),
        "#!/bin/sh\nif [ \"${1:-}\" = merge-file ]; then cat >/dev/null; echo unchanged; exit 0; fi\necho $$ >>\"$PUDDLE_ROOT/agent.pids\"\necho mute agent here\nexec sleep 1000\n",
    )
    .unwrap();
    std::fs::set_permissions(
        fr.path("/puddle/mute"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let mute = BootPlan::builder(&ImageConfig::default())
        .agent(puddle_boot::AgentConfig {
            binary: GuestPath::new("/puddle/mute").unwrap(),
            port: 3999,
        })
        .build()
        .unwrap();
    let out = fr.run(&mute);
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not listening on 127.0.0.1:3999 after 10 s"),
        "{stderr}"
    );
    assert!(stderr.contains("mute agent here"), "{stderr}");
}

#[test]
fn without_sysctl_the_hook_writes_proc_sys_directly() {
    let fr = FakeRoot::new(&shells()[0]);
    let plan = BootPlan::builder(&ImageConfig::default())
        .no_agent()
        .build()
        .unwrap();
    let out = fr.run_raw(&plan.render(), &[], false);
    assert!(out.status.success(), "{}", show(&out));
    assert_eq!(count(&fr.calls(), "sysctl"), 0);
    assert_eq!(fr.read("/proc/sys/fs/inotify/max_user_watches"), "524288\n");
}

#[test]
fn the_hook_reports_the_guests_git_version_when_there_is_a_git() {
    for shell in shells() {
        let fr = FakeRoot::new(&shell);
        let with_git = |version: &str| format!("#!/bin/sh\necho 'git version {version}'\n");
        fr.stub("git", &with_git("2.34.1"));
        let out = fr.run(&full_plan("CA\n"));
        assert!(out.status.success(), "{}", show(&out));
        let report = puddle_boot::BootReport {
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            ..Default::default()
        };
        assert_eq!(report.git_version(), Some((2, 34)));
    }
}

#[test]
fn the_hook_reports_the_kubernetes_tools_on_the_path() {
    for shell in shells() {
        let fr = FakeRoot::new(&shell);
        let tools_of = |fr: &FakeRoot| {
            let out = fr.run(&full_plan("CA\n"));
            assert!(out.status.success(), "{}", show(&out));
            puddle_boot::BootReport {
                stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
                ..Default::default()
            }
            .kubernetes_tools()
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>()
        };
        // Nothing installed: no line, so the host raises nothing.
        assert_eq!(tools_of(&fr), Vec::<String>::new());
        fr.stub("kind", "#!/bin/sh\nexit 0\n");
        fr.stub("minikube", "#!/bin/sh\nexit 0\n");
        assert_eq!(tools_of(&fr), ["kind", "minikube"]);
        // A later run finds a tool added since.
        fr.stub("k3d", "#!/bin/sh\nexit 0\n");
        assert_eq!(tools_of(&fr), ["kind", "k3d", "minikube"]);
    }
}

#[test]
fn a_kubernetes_tool_only_the_images_own_path_names_is_found_too() {
    let fr = FakeRoot::new(&shells()[0]);
    let bin = tempfile::tempdir().unwrap();
    let tool = bin.path().join("minikube");
    std::fs::write(&tool, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&tool, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let config = ImageConfig {
        env: vec![("PATH".into(), bin.path().display().to_string())],
        ..ImageConfig::default()
    };
    let plan = BootPlan::builder(&config).no_agent().build().unwrap();
    let out = fr.run_raw(&plan.render(), &[], false);
    assert!(out.status.success(), "{}", show(&out));
    let report = puddle_boot::BootReport {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        ..Default::default()
    };
    assert_eq!(report.kubernetes_tools(), ["minikube"]);
}

#[test]
fn no_credential_helper_is_ever_written() {
    let fr = FakeRoot::new(&shells()[0]);
    assert!(fr.run(&full_plan("CA\n")).status.success());
    let mut stack = vec![fr.path("/etc"), fr.path("/root")];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let p = entry.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else {
                let text = std::fs::read_to_string(&p)
                    .unwrap_or_default()
                    .to_lowercase();
                assert!(!text.contains("credential"), "{}", p.display());
            }
        }
    }
}

#[test]
fn outside_a_test_root_the_hook_needs_root() {
    let is_root = std::process::Command::new("id")
        .arg("-u")
        .output()
        .is_ok_and(|o| o.stdout == b"0\n");
    if is_root {
        // The refusal can't be shown as root; the CI job runs as a normal user and says so.
        assert!(
            std::env::var_os("PUDDLE_REQUIRE_NON_ROOT").is_none(),
            "PUDDLE_REQUIRE_NON_ROOT is set but the tests run as root"
        );
        return;
    }
    let fr = FakeRoot::new(&shells()[0]);
    let mut cmd = fr.command(&[], true);
    cmd.env("PUDDLE_ROOT", "");
    let out = cmd.output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("must run as root"));
}

/// The whole path: plan rendered by Rust, run by the hook through the fake runtime's exec, gate
/// opened on success.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn boot_hook_through_the_fake_runtime_runs_the_real_script() {
    use puddle_compute::fake::{ExecContext, FakeRuntime};
    use puddle_compute::{ExecOutput, ExecRequest, FileMount, Runtime, SandboxSpec};
    use puddle_types::{ImageRef, SandboxName};
    use std::io::Write as _;

    let fr = std::sync::Arc::new(FakeRoot::new(&shells()[0]));
    let runner = fr.clone();
    let handler = move |_: &mut ExecContext<'_>, r: &ExecRequest| -> Option<ExecOutput> {
        if r.args.first().map(String::as_str) != Some("/puddle/boot.sh") {
            return None;
        }
        assert_eq!(r.user.as_deref(), Some("root"));
        assert_eq!(r.env.get("PUDDLE_ROOT"), Some(""));
        let mut child = runner.command(&r.args[1..], true).spawn().unwrap();
        child.stdin.take().unwrap().write_all(&r.stdin).unwrap();
        let out = child.wait_with_output().unwrap();
        Some(ExecOutput::new(
            out.status.code().unwrap_or(-1),
            out.stdout,
            out.stderr,
        ))
    };
    let rt = FakeRuntime::new();
    rt.on_exec(handler);
    let image = ImageRef::new(FakeRuntime::DEBIAN).unwrap();
    let config = rt.pull_image(&image).await.unwrap();
    let plan = BootPlan::builder(&config).env(&env()).build().unwrap();
    let mounts = puddle_boot::write_assets(&fr.path("/host-assets")).unwrap();
    assert!(mounts.iter().all(|m: &FileMount| m.host.exists()));
    let spec = puddle_boot::with_boot_mounts(
        SandboxSpec::new(SandboxName::new("real").unwrap(), image),
        mounts,
        Some(&fr.path("/puddle/puddle-agent")),
    );
    let gate = Gate::new();
    let sb = BootHook::new()
        .create(&rt, spec, &plan, &gate)
        .await
        .unwrap();
    assert_eq!(gate.state(), GateState::Ready);
    assert!(sb.boot_report().stdout.contains("puddle-boot: ready"));
    assert_eq!(fr.read("/proc/sys/kernel/unprivileged_bpf_disabled"), "1\n");
    assert!(fr.read(ENV_FILE_GUEST).contains("HTTPS_PROXY"));
    assert!(
        fr.read(&format!(
            "/merge-specs/{}",
            MACHINE_SETTINGS_GUEST.replace('/', "_")
        ))
        .contains("autoForwardPortsFallback")
    );
    assert!(
        !fr.path(CA_DIR_GUEST).exists(),
        "no provider sent a CA file"
    );
    sb.stop().await.unwrap();
}

const DOCKER: &str = "/root/.docker/config.json";

fn merged(path: &str) -> GuestFile {
    let spec = MergeSpec::new(
        MergeFormat::Json,
        vec![MergeEntry::json(
            &["proxies", "default"],
            &serde_json::json!({"httpProxy": "http://172.17.0.1:3128"}),
        )],
    )
    .unwrap();
    GuestFile::merged(GuestPath::new(path).unwrap(), spec)
        .with_mode(0o600)
        .unwrap()
}

fn plan_with(files: Vec<GuestFile>) -> BootPlan {
    BootPlan::builder(&ImageConfig::default())
        .files(files)
        .build()
        .unwrap()
}

/// The merge tool's calls for the files a test lists (not VS Code's Machine settings, which
/// every plan merges).
fn merge_calls(fr: &FakeRoot) -> Vec<String> {
    fr.calls()
        .into_iter()
        .filter(|c| c.starts_with("merge-file") && !c.contains(MACHINE_SETTINGS_GUEST))
        .collect()
}

#[test]
fn merged_files_go_through_the_merge_tool_and_lose_only_puddles_keys_when_dropped() {
    for sh in shells() {
        let fr = FakeRoot::new(&sh);
        let file = merged(DOCKER);
        let out = fr.run(&plan_with(vec![file.clone()]));
        assert!(out.status.success(), "{}", show(&out));
        // The log says which file got Puddle's lines.
        assert!(
            String::from_utf8_lossy(&out.stdout).contains(&format!(
                "{DOCKER}: Puddle's own lines were changed (everything else in the file is as it was)"
            )),
            "{}",
            show(&out)
        );
        assert_eq!(
            merge_calls(&fr),
            [format!("merge-file apply {DOCKER} 0600")]
        );
        let puddle_types::ApplyKind::Merge(spec) = file.apply() else {
            panic!("not merged")
        };
        let sent: MergeSpec = serde_json::from_str(&fr.read("/merge-spec")).unwrap();
        assert_eq!(&sent, spec);
        assert!(fr.path("/root/.docker").is_dir());
        assert_eq!(
            fr.read("/var/lib/puddle/merge-files"),
            format!("{MACHINE_SETTINGS_GUEST}\n{DOCKER}\n")
        );
        assert!(!fr.read("/var/lib/puddle/boot-files").contains(DOCKER));

        // The provider stops listing it: the tool removes Puddle's keys; the file isn't deleted
        // by the hook.
        let out = fr.run(&plan_with(vec![]));
        assert!(out.status.success(), "{}", show(&out));
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains(&format!(
                "Puddle's keys removed from {DOCKER} (no longer in the plan): changed"
            )),
            "{stdout}"
        );
        assert_eq!(
            merge_calls(&fr).last().unwrap(),
            &format!("merge-file remove {DOCKER}")
        );
        assert_eq!(
            fr.read("/var/lib/puddle/merge-files"),
            format!("{MACHINE_SETTINGS_GUEST}\n")
        );
        let out = fr.run(&plan_with(vec![]));
        assert!(out.status.success());
        assert_eq!(merge_calls(&fr).len(), 2, "nothing left to remove");
    }
}

#[test]
fn a_file_the_tool_cannot_parse_is_left_and_the_boot_goes_on() {
    let fr = FakeRoot::new(&shells()[0]);
    std::fs::write(
        fr.path("/merge-answer"),
        "left: the file is not valid JSON: EOF while parsing at line 1 column 1\n",
    )
    .unwrap();
    let out = fr.run(&plan_with(vec![merged(DOCKER)]));
    assert!(out.status.success(), "{}", show(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(&format!(
            "{DOCKER} left as it is: the file is not valid JSON: EOF while parsing"
        )),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!("{MACHINE_SETTINGS_GUEST} left as it is")),
        "{stdout}"
    );
    assert!(stdout.contains("files applied (2 changed)"), "{stdout}");
}

#[test]
fn a_failing_or_odd_merge_tool_fails_the_boot_with_its_message() {
    let fr = FakeRoot::new(&shells()[0]);
    std::fs::write(fr.path("/merge-answer"), "fail").unwrap();
    let out = fr.run(&plan_with(vec![merged(DOCKER)]));
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(&format!("cannot merge {MACHINE_SETTINGS_GUEST}: disk full")),
        "{stderr}"
    );
    std::fs::write(fr.path("/merge-answer"), "maybe").unwrap();
    let out = fr.run(&plan_with(vec![merged(DOCKER)]));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unexpected answer from the merge tool"),
        "{stderr}"
    );
    std::fs::remove_file(fr.path("/puddle/puddle-agent")).unwrap();
    let out = fr.run(&plan_with(vec![merged(DOCKER)]));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("the merge tool /puddle/puddle-agent is missing"),
        "{stderr}"
    );
}

#[test]
fn switching_a_file_between_replace_and_merge_keeps_it() {
    let fr = FakeRoot::new(&shells()[0]);
    // The earlier whole-file Docker config, then the merged one (an upgrade): the file stays.
    assert!(
        fr.run(&plan_with(vec![file(DOCKER, b"{}")]))
            .status
            .success()
    );
    let out = fr.run(&plan_with(vec![merged(DOCKER)]));
    assert!(out.status.success(), "{}", show(&out));
    assert_eq!(fr.read(DOCKER), "{}");
    assert_eq!(
        merge_calls(&fr),
        [format!("merge-file apply {DOCKER} 0600")]
    );
    // Back to whole-file: the tool only forgets its record.
    let out = fr.run(&plan_with(vec![file(DOCKER, b"{\"x\":1}")]));
    assert!(out.status.success(), "{}", show(&out));
    assert_eq!(
        merge_calls(&fr).last().unwrap(),
        &format!("merge-file forget {DOCKER}")
    );
    assert_eq!(fr.read(DOCKER), "{\"x\":1}");
}

#[test]
fn without_the_merge_tool_the_removal_waits_for_a_later_boot() {
    let fr = FakeRoot::new(&shells()[0]);
    assert!(fr.run(&plan_with(vec![merged(DOCKER)])).status.success());
    std::fs::rename(fr.path("/puddle/puddle-agent"), fr.path("/agent.saved")).unwrap();
    let bare = BootPlan::builder(&ImageConfig::default())
        .no_agent()
        .build()
        .unwrap();
    // Every plan merges VS Code's Machine settings, so a missing tool now fails the boot
    // before anything is removed: the record of Docker's keys stays for a later boot.
    let out = fr.run(&bare);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("the merge tool /puddle/puddle-agent is missing")
    );
    assert_eq!(
        fr.read("/var/lib/puddle/merge-files"),
        format!("{MACHINE_SETTINGS_GUEST}\n{DOCKER}\n")
    );
    std::fs::rename(fr.path("/agent.saved"), fr.path("/puddle/puddle-agent")).unwrap();
    assert!(fr.run(&bare).status.success());
    assert_eq!(
        merge_calls(&fr).last().unwrap(),
        &format!("merge-file remove {DOCKER}")
    );
}

#[test]
fn provider_steps_run_every_boot_after_the_ca_trigger_and_fail_the_boot_when_they_fail() {
    let step = "/usr/local/lib/puddle/step.sh";
    let plan = |script: &str| {
        BootPlan::builder(&image(&[], &["bash"]))
            .no_agent()
            .file(file(CA, b"CA\n"))
            .file(file(step, script.as_bytes()))
            .step(GuestPath::new(step).unwrap())
            .build()
            .unwrap()
    };
    let ok = "echo step >>\"$PUDDLE_ROOT/calls\"; echo merged\n";
    for sh in shells() {
        let fr = FakeRoot::new(&sh);
        for _ in 0..2 {
            let out = fr.run(&plan(ok));
            assert!(out.status.success(), "{}", show(&out));
            let stdout = String::from_utf8_lossy(&out.stdout);
            assert!(stdout.contains(&format!("step {step}: merged")), "{stdout}");
        }
        // Every boot, after update-ca-certificates (which ran on the first boot only).
        assert_eq!(fr.calls().iter().filter(|c| *c == "step").count(), 2);
        let calls = fr.calls();
        let first_step = calls.iter().position(|c| c == "step").unwrap();
        let update = calls
            .iter()
            .position(|c| c == "update-ca-certificates")
            .unwrap();
        assert!(update < first_step, "{calls:?}");

        let out = fr.run(&plan("echo 'no bundle here' >&2; exit 4\n"));
        assert_eq!(out.status.code(), Some(1), "{}", show(&out));
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains(&format!(
                "puddle-boot: error: step {step} failed: no bundle here"
            )),
            "{stderr}"
        );
        assert!(!fr.path("/run/puddle/boot.done").exists());
    }
}

const STUB_LINE: &str = "nameserver 198.18.0.1";

fn plan(capture: bool) -> BootPlan {
    let builder = BootPlan::builder(&image(&[], &["bash"]));
    if capture {
        builder
    } else {
        builder.no_capture()
    }
    .build()
    .unwrap()
}

#[test]
fn capture_starts_the_agent_with_it_and_puts_the_stub_first_in_resolv_conf() {
    for sh in shells() {
        let fr = FakeRoot::new(&sh);
        let original = "# by the image\nsearch corp.example\nnameserver 10.0.0.2\noptions ndots:2";
        std::fs::write(fr.path("/etc/resolv.conf"), original).unwrap();
        let out = fr.run(&plan(true));
        assert!(out.status.success(), "{}: {}", sh.display(), show(&out));
        assert_eq!(fr.agent_capture(), ["on"]);
        // The stub first; every line the guest had is still there, in order.
        assert_eq!(
            fr.read("/etc/resolv.conf"),
            format!("{STUB_LINE}\n{original}\n")
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.contains("stub DNS answering"), "{stdout}");

        // A second boot changes nothing and does not stack a second line.
        let out = fr.run(&plan(true));
        assert!(out.status.success(), "{}", show(&out));
        assert_eq!(
            fr.read("/etc/resolv.conf").matches(STUB_LINE).count(),
            1,
            "{}",
            fr.read("/etc/resolv.conf")
        );
        assert!(
            !String::from_utf8_lossy(&out.stdout).contains("Puddle's nameserver line"),
            "an unchanged file is not rewritten"
        );
    }
}

#[test]
fn resolv_conf_that_is_a_link_or_missing_is_written_through() {
    let fr = FakeRoot::new(&shells()[0]);
    // A link (systemd-resolved style): the link stays, its target gets the line.
    std::fs::write(fr.path("/etc/real-resolv"), "nameserver 9.9.9.9\n").unwrap();
    std::os::unix::fs::symlink("real-resolv", fr.path("/etc/resolv.conf")).unwrap();
    assert!(fr.run(&plan(true)).status.success());
    assert!(
        std::fs::symlink_metadata(fr.path("/etc/resolv.conf"))
            .unwrap()
            .is_symlink()
    );
    assert_eq!(
        fr.read("/etc/real-resolv"),
        format!("{STUB_LINE}\nnameserver 9.9.9.9\n")
    );

    // No file at all: it is made with the stub alone.
    let fr = FakeRoot::new(&shells()[0]);
    assert!(fr.run(&plan(true)).status.success());
    assert_eq!(fr.read("/etc/resolv.conf"), format!("{STUB_LINE}\n"));
}

#[test]
fn a_stub_that_never_answers_leaves_resolv_conf_alone_and_the_boot_succeeds() {
    let fr = FakeRoot::new(&shells()[0]);
    std::fs::write(fr.path("/etc/resolv.conf"), "nameserver 10.0.0.2\n").unwrap();
    std::fs::write(fr.path("/no-stub"), "").unwrap();
    let started = std::time::Instant::now();
    let out = fr.run(&plan(true));
    assert!(out.status.success(), "{}", show(&out));
    assert!(
        started.elapsed() >= Duration::from_secs(9),
        "waited for the stub for 10 s"
    );
    assert_eq!(fr.read("/etc/resolv.conf"), "nameserver 10.0.0.2\n");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("capture is not up after 10 s"), "{stdout}");
    assert!(stdout.ends_with("puddle-boot: ready\n"), "{stdout}");
}

#[test]
fn capture_off_starts_the_agent_without_it_and_gives_back_the_line() {
    let fr = FakeRoot::new(&shells()[0]);
    std::fs::write(fr.path("/etc/resolv.conf"), "search x\nnameserver 10.0.0.2").unwrap();
    // Off from the start: the file is not touched, not even to add a newline.
    assert!(fr.run(&plan(false)).status.success());
    assert_eq!(fr.agent_capture(), ["off"]);
    assert_eq!(fr.read("/etc/resolv.conf"), "search x\nnameserver 10.0.0.2");

    // On, then off in a later boot: only Puddle's line goes. (A running agent keeps the mode it
    // started with, so each run is a new boot.)
    let reboot = |n: u32| {
        kill9(fr.recorded_pid("supervisor.pid").unwrap());
        for pid in fr.agent_pids() {
            kill9(pid);
        }
        std::fs::write(
            fr.path("/proc/sys/kernel/random/boot_id"),
            format!("boot-{n}\n"),
        )
        .unwrap();
        std::fs::write(fr.path("/proc/net/tcp"), "  sl  local_address\n").unwrap();
        std::fs::write(fr.path("/proc/net/udp"), "  sl  local_address\n").unwrap();
    };
    reboot(2);
    assert!(fr.run(&plan(true)).status.success());
    assert!(fr.read("/etc/resolv.conf").starts_with(STUB_LINE));
    reboot(3);
    assert!(fr.run(&plan(false)).status.success());
    assert_eq!(
        fr.read("/etc/resolv.conf"),
        "search x\nnameserver 10.0.0.2\n"
    );
}

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
            fr.path("/workspaces/w"),
            fr.path("/workspaces/w/api"),
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
        let missing = start_folder_file(&guest(&root), &guest(&fr.path("/workspaces/w/later")));
        std::fs::write(&file, missing.contents()).unwrap();
        assert_eq!(at(true, &home), real(&root), "{why}");
        let nowhere = start_folder_file(
            &guest(&fr.path("/workspaces/none")),
            &guest(&fr.path("/workspaces/none/x")),
        );
        std::fs::write(&file, nowhere.contents()).unwrap();
        assert_eq!(at(true, &home), real(&home), "{why}");

        // The folder's name is one word to the shell, whatever it holds.
        let odd = fr.path("/workspaces/w/it's $(touch pwned) `x`");
        std::fs::create_dir_all(&odd).unwrap();
        let quoted = start_folder_file(&guest(&root), &guest(&odd));
        std::fs::write(&file, quoted.contents()).unwrap();
        assert_eq!(at(true, &home), real(&odd), "{why}");
        assert!(!fr.path("/workspaces/w/pwned").exists());
        assert!(!home.join("pwned").exists());
    }
}
