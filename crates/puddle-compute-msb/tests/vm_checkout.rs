// SPDX-License-Identifier: GPL-3.0-or-later
//! The checkout mount at tier V: the one mount whose source is a folder the user picked, not a
//! file Puddle wrote for guests. A real git checkout on the host has its `.git` mounted into a
//! sandbox with no route, and root in the guest attacks it.
//!
//! - Every write path root has (append, overwrite, truncate, create, mkdir, symlink, hard link,
//!   rename, unlink, chmod) fails, as mounted and again after `mount -o remount,rw` cleared the
//!   guest's flag. The proof is the host's own view of the whole `.git` tree (every entry's kind,
//!   size, mode, modification time, link target and content hash), identical before and after.
//! - Only `.git` crosses: a canary in the working tree (an `.env` file) and one beside the checkout
//!   are not findable anywhere in the guest, and a link inside `.git` that names the host canary
//!   resolves in the guest's own file system, never the host's.
#![expect(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "VM test helpers: a failed step fails the test"
)]

mod support;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use puddle_compute::{ExecOutput, ExecRequest, FileMount, Runtime};
use support::mount_attack::sha256_hex;
use support::{cleanup, path, sh, spec};

/// Where the checkout's `.git` appears in the guest.
const MOUNTED: &str = "/puddle/checkout";

/// Root's write attempts on the mount, one line `<op> <exit>` each; `$1` is the mount point.
const ATTACK: &str = r##"
g=$1
try() { op=$1; shift; ( "$@" ) >/dev/null 2>&1; echo "$op $?"; }
try append sh -c 'echo x >> "$0/config"' "$g"
try overwrite sh -c 'echo x > "$0/HEAD"' "$g"
try truncate truncate -s 0 "$g/config"
try create_file touch "$g/new-file"
try create_in_objects touch "$g/objects/new-object"
try plant_hook sh -c 'printf "#!/bin/sh\necho pwned\n" > "$0/hooks/post-checkout"' "$g"
try mkdir mkdir "$g/new-dir"
try symlink ln -s /etc "$g/new-link"
try hardlink ln "$g/HEAD" "$g/HEAD-link"
try rename mv "$g/config" "$g/config-moved"
try unlink rm -f "$g/HEAD"
try rmdir rm -rf "$g/refs"
try chmod chmod 0777 "$g/config"
try utimes touch -d @1 "$g/config"
"##;

/// One entry of the host's `.git` tree as the host sees it.
#[derive(Debug, PartialEq, Eq)]
struct Entry {
    kind: &'static str,
    len: u64,
    readonly: bool,
    #[cfg(unix)]
    mode: u32,
    modified: Option<SystemTime>,
    link: Option<PathBuf>,
    sha256: Option<String>,
}

/// Every entry below `root`, by relative path, without following links.
fn host_tree(root: &Path) -> BTreeMap<PathBuf, Entry> {
    let mut out = BTreeMap::new();
    let mut todo = vec![root.to_path_buf()];
    while let Some(dir) = todo.pop() {
        for item in std::fs::read_dir(&dir).unwrap() {
            let item = item.unwrap().path();
            let meta = std::fs::symlink_metadata(&item).unwrap();
            let kind = if meta.file_type().is_symlink() {
                "link"
            } else if meta.is_dir() {
                todo.push(item.clone());
                "dir"
            } else {
                "file"
            };
            let sha256 = (kind == "file").then(|| sha256_hex(&std::fs::read(&item).unwrap()));
            out.insert(
                item.strip_prefix(root).unwrap().to_path_buf(),
                Entry {
                    kind,
                    len: meta.len(),
                    readonly: meta.permissions().readonly(),
                    #[cfg(unix)]
                    mode: std::os::unix::fs::PermissionsExt::mode(&meta.permissions()),
                    modified: meta.modified().ok(),
                    link: std::fs::read_link(&item).ok(),
                    sha256,
                },
            );
        }
    }
    out
}

/// Runs `git` on the host to build the fixture (never on anything the guest touched).
fn git(dir: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=Puddle Test",
            "-c",
            "user.email=test@puddle.invalid",
        ])
        .args([
            "-c",
            "init.defaultBranch=main",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git on the host");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A checkout with two commits on `main` and one on `feature`, an `.env` canary in its working
/// tree and a canary file beside it. Returns the canonical path of its `.git`.
fn checkout(base: &Path) -> PathBuf {
    let work = base.join("api");
    std::fs::create_dir_all(&work).unwrap();
    git(&work, &["init", "-q"]);
    std::fs::write(work.join("README.md"), "hello\n").unwrap();
    git(&work, &["add", "README.md"]);
    git(&work, &["commit", "-q", "-m", "first"]);
    git(&work, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(work.join("feature.txt"), "unpushed\n").unwrap();
    git(&work, &["add", "feature.txt"]);
    git(&work, &["commit", "-q", "-m", "feature"]);
    git(&work, &["checkout", "-q", "main"]);
    std::fs::write(work.join(".env"), "CANARY-checkout-env\n").unwrap();
    std::fs::write(
        base.join("CANARY-checkout-beside"),
        "CANARY-checkout-beside\n",
    )
    .unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        base.join("CANARY-checkout-beside"),
        work.join(".git/escape"),
    )
    .unwrap();
    dunce::canonicalize(work.join(".git")).unwrap()
}

#[expect(
    clippy::print_stderr,
    reason = "the guest's answers are the log of the run"
)]
fn logged(label: &str, out: ExecOutput) -> ExecOutput {
    eprintln!(
        "{label}: exit {}\n{}{}",
        out.status.code,
        out.stdout_text(),
        out.stderr_text()
    );
    out
}

async fn root<S: puddle_compute::Sandbox>(sb: &S, label: &str, script: &str) -> ExecOutput {
    logged(label, sh(sb, script).await)
}

async fn attack<S: puddle_compute::Sandbox>(sb: &S, label: &str) -> ExecOutput {
    let request = ExecRequest::new("sh", ["-c", ATTACK, "attack", MOUNTED]).as_user("root");
    logged(label, sb.exec(request).await.expect("exec"))
}

/// Each line `<op> <exit>` of the attack whose exit is 0, that is, a write that went through.
fn succeeded(report: &str) -> Vec<String> {
    report
        .lines()
        .filter(|l| l.split_whitespace().nth(1) == Some("0"))
        .map(str::to_owned)
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vm_a_checkout_mount_is_read_only_to_root_and_only_its_git_directory_crosses() {
    let settings = support::settings();
    let rt = support::runtime(&settings).await;
    let tmp = tempfile::Builder::new()
        .prefix("puddle-vm-checkout-")
        .tempdir()
        .unwrap();
    let base = dunce::canonicalize(tmp.path()).unwrap();
    let git_dir = checkout(&base);
    let before = host_tree(&git_dir);
    assert!(before.contains_key(Path::new("HEAD")), "{before:?}");

    let s =
        spec(&settings, "checkout").with_file_mount(FileMount::checkout(&git_dir, path(MOUNTED)));
    let name = s.name.clone();
    let sb = rt.create(s).await.expect("create with a checkout mount");

    // The guest reads the repository: HEAD, and the feature branch's ref.
    let head = root(
        &sb,
        "read",
        &format!("cat {MOUNTED}/HEAD; ls {MOUNTED}/refs/heads"),
    )
    .await;
    assert_eq!(head.status.code, 0);
    assert!(head.stdout_text().contains("ref: refs/heads/main"));
    assert!(head.stdout_text().contains("feature"));
    let flag = root(
        &sb,
        "flag",
        &format!(r#"awk '$2 == "{MOUNTED}" {{ print $4 }}' /proc/mounts"#),
    )
    .await;
    assert!(
        flag.stdout_text().split(',').any(|o| o.trim() == "ro"),
        "not read-only as mounted: {}",
        flag.stdout_text()
    );

    let first = attack(&sb, "attack as mounted").await;
    root(
        &sb,
        "remount",
        &format!("mount -o remount,rw {MOUNTED}; echo \"remount exit $?\""),
    )
    .await;
    let second = attack(&sb, "attack after the remount").await;
    // The attack must really have run every step, or a silent script would pass.
    for report in [&first, &second] {
        assert_eq!(report.stdout_text().lines().count(), 14, "{report:?}");
    }

    // What the guest can see of the host: neither canary anywhere, and the link inside `.git`
    // resolves in the guest (where the host path does not exist).
    let found = root(
        &sb,
        "canaries",
        r"find / \( -path /proc -o -path /sys \) -prune -o \( -name 'CANARY-checkout-*' -o -name .env -o -name feature.txt \) -print 2>/dev/null",
    )
    .await;
    #[cfg(unix)]
    let escape = root(
        &sb,
        "escape link",
        &format!("cat {MOUNTED}/escape 2>&1; echo \"exit $?\""),
    )
    .await;

    cleanup(&rt, &[&name]).await;
    let after = host_tree(&git_dir);

    let mut failures = Vec::new();
    for (label, report) in [("as mounted", &first), ("after the remount", &second)] {
        for line in succeeded(&report.stdout_text()) {
            failures.push(format!("{label}: {line} (the write went through)"));
        }
    }
    if before != after {
        for (p, e) in &before {
            if after.get(p) != Some(e) {
                failures.push(format!(
                    "host entry {} changed: {e:?} -> {:?}",
                    p.display(),
                    after.get(p)
                ));
            }
        }
        for p in after.keys().filter(|p| !before.contains_key(*p)) {
            failures.push(format!("host entry {} appeared", p.display()));
        }
    }
    if !found.stdout_text().trim().is_empty() {
        failures.push(format!(
            "a canary is visible in the guest: {}",
            found.stdout_text()
        ));
    }
    #[cfg(unix)]
    if escape.stdout_text().contains("CANARY") {
        failures.push(format!(
            "the link in .git read the host's file: {}",
            escape.stdout_text()
        ));
    }
    assert!(failures.is_empty(), "{failures:#?}");
}
