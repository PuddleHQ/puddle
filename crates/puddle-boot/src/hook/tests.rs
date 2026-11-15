// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn the_report_lists_the_files_the_hook_put_back() {
    let report = BootReport {
        duration: Duration::ZERO,
        stdout: "puddle-boot: sysctls set\npuddle-boot: replaced /etc/a b\n\
                     puddle-boot: replaced /etc/c\npuddle-boot: ready\n"
            .into(),
    };
    assert_eq!(report.replaced_files(), ["/etc/a b", "/etc/c"]);
    assert_eq!(BootReport::default().replaced_files(), [] as [&str; 0]);
}

#[test]
fn the_report_names_an_entrypoint_that_failed_at_once() {
    let said = |stdout: &str| BootReport {
        duration: Duration::ZERO,
        stdout: stdout.into(),
    };
    assert_eq!(
            said("puddle-boot: entrypoint chained: x\npuddle-boot: entrypoint exited with status 127: x: not found\n")
                .entrypoint_failure(),
            Some("127: x: not found")
        );
    assert_eq!(
        said("puddle-boot: entrypoint chained: x\n").entrypoint_failure(),
        None
    );
    assert_eq!(BootReport::default().entrypoint_failure(), None);
}

#[test]
fn the_report_says_when_the_session_user_has_no_sudo() {
    let said = |stdout: &str| BootReport {
        duration: Duration::ZERO,
        stdout: stdout.into(),
    };
    assert!(
        said(
            "puddle-boot: session user agent (uid 1000), no sudo in this image\n\
                 puddle-boot: session user cannot become root: no sudo\n"
        )
        .session_user_has_no_sudo()
    );
    assert!(
        !said("puddle-boot: session user agent (uid 1000), sudo at /usr/bin/sudo\n")
            .session_user_has_no_sudo()
    );
    assert!(
        !said("puddle-boot: session user cannot become root: no sudo, x\n")
            .session_user_has_no_sudo()
    );
    assert!(!BootReport::default().session_user_has_no_sudo());
}

#[test]
fn the_report_names_the_kubernetes_tools_the_hook_found() {
    let said = |stdout: &str| BootReport {
        duration: Duration::ZERO,
        stdout: stdout.into(),
    };
    assert_eq!(
        said("puddle-boot: ready\npuddle-boot: kubernetes tools kind minikube\n")
            .kubernetes_tools(),
        ["kind", "minikube"]
    );
    // A name the hook never looks for is not taken on trust.
    assert_eq!(
        said("puddle-boot: kubernetes tools k3d helm\n").kubernetes_tools(),
        ["k3d"]
    );
    assert_eq!(
        said("puddle-boot: ready\n").kubernetes_tools(),
        [] as [&str; 0]
    );
    assert_eq!(BootReport::default().kubernetes_tools(), [] as [&str; 0]);
}

#[test]
fn the_report_names_the_guests_git_version() {
    let said = |stdout: &str| BootReport {
        duration: Duration::ZERO,
        stdout: stdout.into(),
    };
    assert_eq!(
        said("puddle-boot: git version 2.39.5\n").git_version(),
        Some((2, 39))
    );
    assert_eq!(
        said("puddle-boot: git version 2.34.1.windows.1\n").git_version(),
        Some((2, 34))
    );
    assert_eq!(
        said("puddle-boot: git version 2.39.3 (Apple Git-146)\n").git_version(),
        Some((2, 39))
    );
    assert_eq!(
        said("puddle-boot: git version unknown\n").git_version(),
        None
    );
    assert_eq!(said("puddle-boot: ready\n").git_version(), None);
}

#[test]
fn classify_sorts_outcomes() {
    assert_eq!(
        classify(Ok(ExecOutput::new(0, "puddle-boot: ready\n", ""))),
        Ok("puddle-boot: ready\n".into())
    );
    assert_eq!(
        classify(Ok(ExecOutput::new(1, "", "puddle-boot: error: x\n"))),
        Err(BootFailure::Exited {
            code: 1,
            stderr: "puddle-boot: error: x".into()
        })
    );
    assert!(matches!(
        classify(Ok(ExecOutput::new(127, "", "exec: /bin/sh: not found"))),
        Err(BootFailure::NoShell { .. })
    ));
    assert!(matches!(
        classify(Ok(ExecOutput::new(
            127,
            "",
            "bash: /puddle/boot.sh: No such file or directory"
        ))),
        Err(BootFailure::Exited { code: 127, .. })
    ));
    let spawn = ComputeError::Runtime {
        op: "exec",
        message: "spawn /bin/sh: No such file or directory (os error 2)".into(),
    };
    let err = classify(Err(spawn)).unwrap_err();
    assert!(
        err.to_string().contains("no POSIX shell at /bin/sh"),
        "{err}"
    );
    let timeout = ComputeError::ExecTimeout {
        sandbox: "a".into(),
        program: "/bin/sh".into(),
        timeout: Duration::from_secs(1),
    };
    assert_eq!(
        classify(Err(timeout.clone())),
        Err(BootFailure::Exec(timeout))
    );
}

#[test]
fn stderr_keeps_its_end() {
    let long = format!("{}é{}", "a".repeat(STDERR_LIMIT), "the error");
    let t = tail(&long);
    assert!(t.starts_with("..."));
    assert!(t.ends_with("the error"));
    assert!(t.len() <= STDERR_LIMIT + 3);
    assert_eq!(tail("short"), "short");
}

#[test]
fn hook_settings() {
    let h = BootHook::default()
        .with_timeout(Duration::from_secs(1))
        .with_ready_wait(Duration::from_secs(2));
    assert_eq!(h.timeout, Duration::from_secs(1));
    assert_eq!(h.ready_wait, Duration::from_secs(2));
    assert_eq!(BootHook::new().timeout, BootHook::DEFAULT_TIMEOUT);
}
