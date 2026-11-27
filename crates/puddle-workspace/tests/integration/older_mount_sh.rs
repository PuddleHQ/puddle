// SPDX-License-Identifier: AGPL-3.0-or-later
//! The boot step `Layout::older_mount_step` writes (`guest/older-mount.sh` behind the older mount
//! point), under dash and bash, against a fake root, a fake mountinfo and a `mount` stub that
//! records its arguments: a test is not root and cannot mount.
#![cfg(unix)]
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]

use std::path::PathBuf;
use std::process::Output;

use puddle_types::GuestPath;
use puddle_workspace::Layout;

use crate::common::{command, shells};

struct Fixture {
    tmp: tempfile::TempDir,
    step: PathBuf,
    mountinfo: PathBuf,
    mount: PathBuf,
    log: PathBuf,
}

/// The step for `/workspaces/acme`, a mountinfo with `mounted` as mount points, and a `mount`
/// stub that exits `mount_exit`.
fn fixture(mounted: &[&str], mount_exit: u8) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let step_file = Layout::new()
        .unwrap()
        .older_mount_step(&GuestPath::new("/workspaces/acme").unwrap())
        .unwrap();
    let step = tmp.path().join("older-mount.sh");
    std::fs::write(&step, step_file.contents()).unwrap();
    let mountinfo = tmp.path().join("mountinfo");
    let lines: String = std::iter::once("22 1 0:20 / / rw,relatime - overlay overlay rw")
        .chain(mounted.iter().copied())
        .enumerate()
        .map(|(n, point)| {
            if n == 0 {
                format!("{point}\n")
            } else {
                format!(
                    "{} 22 254:1 / {point} rw,relatime - ext4 /dev/vdb rw\n",
                    30 + n
                )
            }
        })
        .collect();
    std::fs::write(&mountinfo, lines).unwrap();
    let log = tmp.path().join("mount.log");
    let mount = tmp.path().join("mount");
    puddle_test_kit::write_executable(
        &mount,
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >>'{}'\nexit {mount_exit}\n",
            log.display()
        ),
    );
    Fixture {
        tmp,
        step,
        mountinfo,
        mount,
        log,
    }
}

fn run(shell: &str, fx: &Fixture) -> Output {
    command(shell)
        .arg(&fx.step)
        .env("PUDDLE_ROOT", fx.tmp.path())
        .env("PUDDLE_MOUNTINFO", &fx.mountinfo)
        .env("PUDDLE_MOUNT", &fx.mount)
        .output()
        .unwrap()
}

fn calls(fx: &Fixture) -> Vec<String> {
    std::fs::read_to_string(&fx.log)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn binds_the_older_mount_point_at_the_workspace_folder() {
    for shell in shells() {
        let fx = fixture(&["/workspaces/acme"], 0);
        let out = run(shell, &fx);
        assert!(out.status.success(), "{shell}: {out:?}");
        let root = fx.tmp.path().display();
        assert_eq!(
            calls(&fx),
            [format!("-o bind {root}/workspaces/acme {root}/workspace")],
            "{shell}"
        );
        assert!(fx.tmp.path().join("workspace").is_dir(), "{shell}");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            "the volume at /workspaces/acme is at /workspace too",
            "{shell}"
        );
    }
}

#[test]
fn a_second_run_in_the_same_boot_mounts_nothing() {
    for shell in shells() {
        let fx = fixture(&["/workspaces/acme", "/workspace"], 0);
        let out = run(shell, &fx);
        assert!(out.status.success(), "{shell}: {out:?}");
        assert!(calls(&fx).is_empty(), "{shell}: {:?}", calls(&fx));
    }
}

#[test]
fn no_volume_at_the_older_mount_point_fails_the_boot_without_a_mount() {
    for shell in shells() {
        let fx = fixture(&[], 0);
        let out = run(shell, &fx);
        assert_eq!(out.status.code(), Some(1), "{shell}: {out:?}");
        assert!(calls(&fx).is_empty(), "{shell}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("not mounted at /workspaces/acme"),
            "{shell}: {out:?}"
        );
    }
}

#[test]
fn a_failed_mount_fails_the_boot_and_says_so() {
    for shell in shells() {
        let fx = fixture(&["/workspaces/acme"], 32);
        let out = run(shell, &fx);
        assert_eq!(out.status.code(), Some(1), "{shell}: {out:?}");
        assert!(
            String::from_utf8_lossy(&out.stderr)
                .contains("cannot bind /workspaces/acme at /workspace"),
            "{shell}: {out:?}"
        );
    }
}
