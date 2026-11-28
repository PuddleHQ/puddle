// SPDX-License-Identifier: AGPL-3.0-or-later
//! The image entrypoint the hook chains, and how its failures are reported.

use super::*;

#[test]
fn entrypoint_is_chained_once_with_puddle_env_and_workdir() {
    for sh in shells() {
        let fr = FakeRoot::new(&sh);
        let entry = fr.path("/entry.sh");
        puddle_test_kit::write_executable(
            &entry,
            "#!/bin/sh\nprintf '%s|%s|%s\\n' \"$HTTPS_PROXY\" \"$*\" \"$(pwd)\" >>\"$PUDDLE_ROOT/entry.out\"\nexec sleep 1000\n",
        );
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

#[test]
fn an_entrypoint_that_already_exited_is_not_started_again_by_a_hook_rerun() {
    for sh in shells() {
        let fr = FakeRoot::new(&sh);
        let entry = fr.path("/entry.sh");
        puddle_test_kit::write_executable(
            &entry,
            "#!/bin/sh\necho ran >>\"$PUDDLE_ROOT/entry.out\"\nexit 0\n",
        );
        let plan = BootPlan::builder(&image(&[entry.display().to_string()], &[]))
            .build()
            .unwrap();
        let out = fr.run(&plan);
        assert!(out.status.success(), "{}", show(&out));
        eventually_blocking("the entrypoint finishes", Duration::from_secs(5), || {
            fr.path("/run/puddle/entrypoint.exit").exists()
        });
        let out = fr.run(&plan);
        assert!(out.status.success(), "{}", show(&out));
        assert_eq!(fr.read("/entry.out"), "ran\n", "{}", show(&out));
    }
}

/// Boots `script` as the image's entrypoint and returns what the hook said.
fn entrypoint_report(sh: &std::path::Path, script: &str) -> String {
    let fr = FakeRoot::new(sh);
    let entry = fr.path("/entry.sh");
    puddle_test_kit::write_executable(&entry, script);
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
        puddle_test_kit::write_executable(&entry, "#!/nonexistent/interpreter\n");
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
    puddle_test_kit::write_executable(&entry, script);
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
