// SPDX-License-Identifier: AGPL-3.0-or-later
//! The real `guest/boot.sh` run as root against a fake root folder, so the ownership calls it
//! only records when it is not root (`own()`, the `chown 0:0` of each written file) really run
//! and their results are read back with `stat`. The other boot tests (`boot_sh`) run as a normal
//! user and cannot show this.
//!
//! The CI step `boot.sh as root` builds this binary as the runner user and runs it with
//! `sudo`, with `PUDDLE_REQUIRE_ROOT=1`. Without that variable every test returns at once, so a
//! plain `cargo test` stays green on a box without root.
#![cfg(unix)]
#![expect(
    clippy::unwrap_used,
    reason = "test helpers outside #[test] fns: a failed setup fails the test"
)]

#[path = "common/mod.rs"]
#[expect(
    dead_code,
    reason = "the shared harness has helpers this binary does not use"
)]
mod common;

use std::collections::BTreeMap;
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;

use common::{FakeRoot, shells, show};
use puddle_boot::BootPlan;
use puddle_compute::ImageConfig;
use puddle_types::{GuestFile, GuestPath, SessionUser};

/// Whether this run is the root lane. Panics when the lane asked for root (`PUDDLE_REQUIRE_ROOT`)
/// and the tests are not root, so a lane that silently ran unprivileged fails.
fn root_lane() -> bool {
    if std::env::var_os("PUDDLE_REQUIRE_ROOT").is_none() {
        return false;
    }
    let is_root = std::process::Command::new("id")
        .arg("-u")
        .output()
        .is_ok_and(|o| o.stdout == b"0\n");
    assert!(
        is_root,
        "PUDDLE_REQUIRE_ROOT is set but the tests do not run as root"
    );
    true
}

fn user_database(fr: &FakeRoot) {
    std::fs::write(
        fr.path("/etc/passwd"),
        "root:x:0:0:root:/root:/bin/bash\nvscode:x:1000:1000::/home/vscode:/bin/bash\n",
    )
    .unwrap();
    std::fs::write(fr.path("/etc/group"), "root:x:0:\nvscode:x:1000:\n").unwrap();
    std::fs::write(fr.path("/etc/shadow"), "root:*:19000:0:99999:7:::\n").unwrap();
}

fn file(path: &str) -> GuestFile {
    GuestFile::new(GuestPath::new(path).unwrap(), b"x\n".to_vec())
}

fn plan(user: SessionUser, files: &[&str]) -> BootPlan {
    let mut builder = BootPlan::builder(&ImageConfig::default())
        .session_user(user)
        .no_agent();
    for path in files {
        builder = builder.file(file(path));
    }
    builder.build().unwrap()
}

/// The plan for the session user `agent`: a file below its home and one in `/etc`.
fn agent_plan() -> BootPlan {
    plan(
        SessionUser::Agent,
        &["/home/agent/.gradle/init.d/a.gradle", "/etc/npmrc"],
    )
}

/// `<uid>:<gid>` of `path` from `stat`, not following a final symlink.
fn owner(path: &Path) -> String {
    let meta = std::fs::symlink_metadata(path).unwrap();
    format!("{}:{}", meta.uid(), meta.gid())
}

/// The owner of the home folder and everything below it.
fn home_owners(fr: &FakeRoot, home: &str) -> BTreeMap<String, String> {
    fn walk(dir: &Path, out: &mut BTreeMap<String, String>) {
        out.insert(dir.display().to_string(), owner(dir));
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else {
                out.insert(path.display().to_string(), owner(&path));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(&fr.path(home), &mut out);
    out
}

#[test]
fn the_session_users_home_and_files_below_it_belong_to_that_user() {
    if !root_lane() {
        return;
    }
    for sh in shells() {
        let fr = FakeRoot::new(&sh);
        user_database(&fr);
        let out = fr.run(&agent_plan());
        assert!(out.status.success(), "{}: {}", sh.display(), show(&out));
        for path in [
            "/home/agent",
            "/home/agent/.gradle",
            "/home/agent/.gradle/init.d",
            "/home/agent/.gradle/init.d/a.gradle",
        ] {
            assert_eq!(owner(&fr.path(path)), "1001:1001", "{path}");
        }
        // Files the plan did not give away stay root's.
        assert_eq!(owner(&fr.path("/etc/npmrc")), "0:0");
    }
}

#[test]
fn every_file_of_a_root_session_plan_belongs_to_root() {
    if !root_lane() {
        return;
    }
    for sh in shells() {
        let fr = FakeRoot::new(&sh);
        user_database(&fr);
        let plan = plan(
            SessionUser::Root,
            &["/etc/npmrc", "/root/.gradle/init.d/a.gradle"],
        );
        let out = fr.run(&plan);
        assert!(out.status.success(), "{}: {}", sh.display(), show(&out));
        // The merged settings file is the agent's to write, not this hook's.
        for path in [
            "/etc/npmrc",
            "/root/.gradle/init.d/a.gradle",
            "/etc/profile.d/01-puddle-env.sh",
            "/etc/puddle/gitconfig",
        ] {
            assert_eq!(owner(&fr.path(path)), "0:0", "{path}");
        }
    }
}

#[test]
fn a_failing_chown_fails_the_boot_and_says_what_it_could_not_give() {
    if !root_lane() {
        return;
    }
    for sh in shells() {
        let fr = FakeRoot::new(&sh);
        user_database(&fr);
        fr.stub("chown", "#!/bin/sh\nexit 1\n");
        let out = fr.run(&agent_plan());
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            out.status.code(),
            Some(1),
            "{}: {}",
            sh.display(),
            show(&out)
        );
        assert!(
            stderr.contains("error: cannot give ") && stderr.contains(" to 1001:1001"),
            "{}: {}",
            sh.display(),
            show(&out)
        );

        // A root plan has no `own()` call: the failing step is the one that makes root own a file.
        let fr = FakeRoot::new(&sh);
        user_database(&fr);
        fr.stub("chown", "#!/bin/sh\nexit 1\n");
        let out = fr.run(&plan(SessionUser::Root, &["/etc/npmrc"]));
        assert_eq!(
            out.status.code(),
            Some(1),
            "{}: {}",
            sh.display(),
            show(&out)
        );
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("error: cannot make root own "),
            "{}: {}",
            sh.display(),
            show(&out)
        );
    }
}

#[test]
fn a_second_boot_leaves_the_same_owners() {
    if !root_lane() {
        return;
    }
    for sh in shells() {
        let fr = FakeRoot::new(&sh);
        user_database(&fr);
        let plan = agent_plan();
        let out = fr.run(&plan);
        assert!(out.status.success(), "{}: {}", sh.display(), show(&out));
        let first = home_owners(&fr, "/home/agent");
        let etc = owner(&fr.path("/etc/npmrc"));
        let out = fr.run(&plan);
        assert!(out.status.success(), "{}: {}", sh.display(), show(&out));
        assert_eq!(home_owners(&fr, "/home/agent"), first);
        assert_eq!(owner(&fr.path("/etc/npmrc")), etc);
    }
}

#[test]
fn the_runs_touch_nothing_outside_the_fake_root() {
    if !root_lane() {
        return;
    }
    let fr = FakeRoot::new(&shells()[0]);
    user_database(&fr);
    let out = fr.run(&plan(SessionUser::Root, &["/etc/npmrc"]));
    assert!(out.status.success(), "{}", show(&out));
    for real in ["/run/puddle", "/var/lib/puddle"] {
        assert!(!Path::new(real).exists(), "{real} exists on the host");
    }
}
