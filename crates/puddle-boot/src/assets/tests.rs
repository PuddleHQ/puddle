// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn constants_are_valid_guest_paths_under_the_mount_dir() {
    let dir = guest_path(MOUNT_DIR_GUEST);
    for p in [
        BOOT_SH_GUEST,
        AGENT_SUPERVISE_GUEST,
        AGENT_GUEST,
        CODE_SERVER_GUEST,
    ] {
        assert!(guest_path(p).is_within(&dir), "{p}");
    }
}

#[test]
fn ssh_runs_the_agent_where_the_hook_mounts_it() {
    // The guest's ssh_config names the agent by path; puddle-guest-env can't see this crate.
    assert_eq!(puddle_guest_env::DEFAULT_AGENT, AGENT_GUEST);
}

#[test]
#[should_panic(expected = "is not valid")]
fn an_invalid_constant_is_a_bug() {
    let _ = guest_path("relative");
}

#[test]
fn scripts_are_posix_sh_with_licence_headers() {
    for s in [BOOT_SH, AGENT_SUPERVISE_SH] {
        assert!(s.starts_with("#!/bin/sh\n# SPDX-License-Identifier: AGPL-3.0-or-later\n"));
        assert!(!s.contains('\r'), "CRLF would break sh in the guest");
    }
}

#[test]
fn read_only_takes_regular_files_puddle_wrote() {
    // dash's `read` takes one byte per read(2) and procfs answers a read at a non-zero
    // offset with EOF: `read x </proc/sys/...` sees "1" for 1024 (found on msb 0.7.7).
    // The fake-root tests use regular files and can't catch it, so `read` may only redirect
    // from the plan or Puddle's own pid files; procfs values go through `cat`.
    for (name, script) in [
        ("boot.sh", BOOT_SH),
        ("agent-supervise.sh", AGENT_SUPERVISE_SH),
    ] {
        for line in script.lines().filter(|l| l.contains("read ")) {
            if let Some((_, target)) = line.split_once(" <") {
                let target = target.split_whitespace().next().unwrap_or_default();
                assert!(
                    ["\"$PLAN\"", "\"$1\"", "\"$envfile\""].contains(&target),
                    "{name}: `{}` reads {target} with `read`",
                    line.trim()
                );
            }
        }
    }
}
