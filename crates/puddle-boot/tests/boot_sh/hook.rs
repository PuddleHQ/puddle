// SPDX-License-Identifier: AGPL-3.0-or-later
//! The hook itself: failures, boot.d steps, probes and the fake-runtime run.

use super::*;

/// `plan` with `lines` run by the hook just before the plan ends.
fn plan_with_lines(plan: &BootPlan, lines: &str) -> Vec<u8> {
    let text = String::from_utf8(plan.render()).unwrap();
    let at = text.rfind("puddle_plan_end").unwrap();
    format!("{}{lines}\n{}", &text[..at], &text[at..]).into_bytes()
}

#[test]
fn a_record_the_hook_cannot_append_to_fails_the_boot_with_its_name() {
    let plan = BootPlan::builder(&ImageConfig::default())
        .no_agent()
        .build()
        .unwrap();
    // Each case turns the file into a directory, so the append fails, then calls what appends.
    let cases = [
        (
            "triggers",
            "puddle_on_change a b",
            "cannot record a trigger",
        ),
        ("steps", "puddle_step x", "cannot record a step"),
        (
            "agent.env.new",
            "puddle_agent_env K V",
            "cannot record the agent's settings",
        ),
        ("agent.env.new", ":", "cannot record the agent's settings"),
        (
            "entry-env",
            "puddle_entrypoint_env K=V",
            "cannot record the entrypoint environment",
        ),
        (
            "files.new",
            "puddle_file /etc/x 644 hi",
            "cannot record a written file",
        ),
    ];
    for (name, call, said) in cases {
        let fr = FakeRoot::new(&shells()[0]);
        let lines = format!("rm -f \"$RUN/{name}\"; mkdir \"$RUN/{name}\"; {call}");
        let out = fr.run_raw(&plan_with_lines(&plan, &lines), &[], true);
        assert_eq!(out.status.code(), Some(1), "{name}: {}", show(&out));
        assert!(
            String::from_utf8_lossy(&out.stderr).contains(said),
            "{name}: {}",
            show(&out)
        );
        assert!(!fr.path("/run/puddle/boot.done").exists(), "{name}");
    }
}

#[test]
fn the_lists_of_written_and_merged_files_are_replaced_whole_or_not_at_all() {
    let plan = full_plan("CA\n");
    for (name, said) in [
        ("boot-files", "cannot record the files written"),
        ("merge-files", "cannot record the files merged"),
    ] {
        let fr = FakeRoot::new(&shells()[0]);
        assert!(fr.run(&plan).status.success());
        let before = fr.read(&format!("/var/lib/puddle/{name}"));
        // The temporary sits next to the target, so a blocked temporary stops the replacement.
        let tmp = fr.path(&format!("/var/lib/puddle/{name}.new"));
        std::fs::create_dir(&tmp).unwrap();
        let out = fr.run(&plan);
        assert_eq!(out.status.code(), Some(1), "{name}: {}", show(&out));
        assert!(
            String::from_utf8_lossy(&out.stderr).contains(said),
            "{name}: {}",
            show(&out)
        );
        assert_eq!(
            fr.read(&format!("/var/lib/puddle/{name}")),
            before,
            "{name}"
        );
        std::fs::remove_dir_all(&tmp).unwrap();
        assert!(fr.run(&plan).status.success(), "{name}");
        assert!(!tmp.exists(), "{name}: no temporary is left behind");
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

    puddle_test_kit::write_executable(
        &fr.path("/puddle/mute"),
        "#!/bin/sh\nif [ \"${1:-}\" = merge-file ]; then cat >/dev/null; echo unchanged; exit 0; fi\necho $$ >>\"$PUDDLE_ROOT/agent.pids\"\necho mute agent here\nexec sleep 1000\n",
    );
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
        // The image's PATH is an empty directory: the usual one (/usr/local/bin) holds kind and
        // minikube on a CI runner, which would be found as if installed in the guest.
        let empty = tempfile::tempdir().unwrap();
        let config = ImageConfig {
            env: vec![("PATH".into(), empty.path().display().to_string())],
            ..ImageConfig::default()
        };
        let plan = BootPlan::builder(&config).no_agent().build().unwrap();
        let tools_of = |fr: &FakeRoot| {
            let out = fr.run_raw(&plan.render(), &[], true);
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
    puddle_test_kit::write_executable(&tool, "#!/bin/sh\nexit 0\n");
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

    let fr = std::sync::Arc::new(FakeRoot::new(&shells()[0]));
    let runner = fr.clone();
    let handler = move |_: &mut ExecContext<'_>, r: &ExecRequest| -> Option<ExecOutput> {
        if r.args.first().map(String::as_str) != Some("/puddle/boot.sh") {
            return None;
        }
        assert_eq!(r.user.as_deref(), Some("root"));
        assert_eq!(r.env.get("PUDDLE_ROOT"), Some(""));
        let mut child = runner.command(&r.args[1..], true).spawn().unwrap();
        puddle_test_kit::feed_stdin(&mut child, &r.stdin);
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
