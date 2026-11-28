// SPDX-License-Identifier: AGPL-3.0-or-later
//! The capture stub in resolv.conf and the agent switch that goes with it.

use super::*;

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
