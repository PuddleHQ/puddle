// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use crate::source::{Commit, Firmware};

fn facts(commits: Vec<Commit>) -> ForkFacts {
    ForkFacts {
        fork_repo: "PuddleHQ/microsandbox".into(),
        upstream_repo: "superradcompany/microsandbox".into(),
        tag: "v0.7.7-puddle.1".into(),
        upstream_tag: "v0.7.7".into(),
        commits,
        firmware: Firmware {
            libkrunfw_version: "5.6.1".into(),
            kernel_version: "6.12.109".into(),
            repo_url: "https://github.com/superradcompany/libkrunfw".into(),
            commit: "21f169f7e94798c8916315f8ac6f5999d5b88566".into(),
            fork_tag: None,
        },
    }
}

fn windows(commits: Vec<Commit>) -> String {
    render(
        &facts(commits),
        HostOs::Windows,
        HostOs::Windows.runtime_files().libkrunfw,
    )
}

#[test]
fn lists_every_fork_commit_and_the_offer() {
    let text = windows(vec![
        Commit {
            sha: "a".repeat(40),
            subject: "fix(vsock): write queued bytes".into(),
        },
        Commit {
            sha: "b".repeat(40),
            subject: "fix(ssh): signal exit".into(),
        },
    ]);
    assert!(text.contains(&format!(
        "  {} fix(vsock): write queued bytes\n",
        "a".repeat(40)
    )));
    assert!(text.contains(&format!("  {} fix(ssh): signal exit\n", "b".repeat(40))));
    assert!(text.contains("https://github.com/PuddleHQ/microsandbox at tag v0.7.7-puddle.1"));
    assert!(text.contains("Apache-2.0 section 4(b)"));
    assert!(text.contains("libkrunfw 5.6.1"));
    assert!(text.contains("https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-6.12.109.tar.xz"));
    assert!(text.contains(
        "https://github.com/superradcompany/libkrunfw/tree/21f169f7e94798c8916315f8ac6f5999d5b88566"
    ));
    assert!(text.contains("at least three years"));
    assert!(text.contains(SOURCE_REQUEST_CONTACT));
    assert!(!text.contains("(none:"));
}

#[test]
fn says_so_when_there_are_no_changes() {
    assert!(windows(vec![]).contains("(none: this build is upstream's code unchanged)"));
}

#[test]
fn names_the_files_of_the_runtime_it_describes() {
    let win = windows(vec![]);
    assert!(win.contains("msb.exe is built from") && win.contains("msb.exe statically links"));
    assert!(win.contains("(libkrunfw.dll) is libkrunfw 5.6.1, built from the source below by the release v0.7.7-puddle.1 of PuddleHQ/microsandbox"));
    assert!(!win.contains("unchanged. The"));
    assert!(win.contains("config-libkrunfw-windows_x86_64"));

    let linux = render(
        &facts(vec![]),
        HostOs::Linux,
        "libkrunfw.so.5 and its copy libkrunfw.so.5.6.1",
    );
    assert!(linux.contains("msb is built from") && linux.contains("msb statically links"));
    assert!(!linux.contains(".exe") && !linux.contains("windows_x86_64"));
    assert!(linux.contains("(libkrunfw.so.5 and its copy libkrunfw.so.5.6.1) is libkrunfw 5.6.1"));
}
