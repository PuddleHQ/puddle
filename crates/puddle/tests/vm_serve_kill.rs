// SPDX-License-Identifier: AGPL-3.0-or-later
//! A hard kill of the real `puddle serve` process on a real microVM (tier K, Linux KVM), the
//! steps S2 and S3 of the "bad-luck day" playbook (docs/qa/07-crash-disk-offline.md) with no
//! person: the built `puddle` program runs from a program folder laid out as an install is
//! (`<folder>/puddle`, `<folder>/runtime/msb`), with its own data folder, and the test drives it
//! through its API as the window does.
//!
//! The workspace holds a repository with 21 commits, a staged and an unstaged file. Then `SIGKILL`
//! ends `puddle serve` with no shutdown. The bar:
//! - every process `puddle serve` had started (msb's VM processes among them) is gone within 5 s;
//! - the next `puddle serve` lists the workspace `crashed` or `stopped`, and a start runs it;
//! - the repository still has its 21 commits and `git fsck --full` finds nothing;
//! - the staged file is still staged, or the problems list says that staged changes were lost.
//!
//! `puddle serve` is the dormant `serve` cargo feature: the vm-linux archive is built with
//! `--features puddle/serve`, and without it this test fails and says so.
#![cfg(target_os = "linux")]
#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "VM test: a failed step fails the test (a missing key in the API's JSON too); progress goes to the log"
)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use puddle_test_kit::eventually_async;
use puddle_vm_tests::{Settings, test_owner};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;

/// The bar for a hard kill: every process Puddle started is gone within 5 s.
const KILL_BAR: Duration = Duration::from_secs(5);
/// The first create pulls the image.
const CREATE_WAIT: Duration = Duration::from_mins(10);
const START_WAIT: Duration = Duration::from_mins(5);
/// The repository in the workspace: one first commit and 20 more.
const COMMITS: u32 = 21;

/// The work worth losing, as the playbook's S1 makes it, in the workspace's folder. `sync` puts the commits on the volume; the kill comes after it, so the
/// commits are the work that must survive, and the staged file is the work that may not.
const MAKE_WORK: &str = r#"set -eu
cd "$1"
git init -q -b main qa-crash
cd qa-crash
git config user.name 'Puddle Test'
git config user.email test@puddle.invalid
git config commit.gpgsign false
echo 0 > qa.txt && git add qa.txt && git commit -qm 'qa 0'
i=1
while [ "$i" -le 20 ]; do
  echo "$i" >> qa.txt && git add qa.txt && git commit -qm "qa $i"
  i=$((i + 1))
done
echo staged > staged.txt && git add staged.txt
echo unstaged > unstaged.txt
sync
git rev-list --count HEAD
"#;

/// What the repository holds after the restart: the commit count, fsck's verdict, whether the
/// staged file is still staged and whether the unstaged one is there.
const READ_WORK: &str = r#"set -u
cd "$1/qa-crash" || exit 3
echo "count $(git rev-list --count HEAD)"
if out=$(git fsck --full 2>&1); then echo "fsck clean"; else echo "fsck failed: $out"; fi
if git diff --cached --name-only | grep -qx staged.txt; then echo "staged kept"; else echo "staged lost"; fi
if [ -f unstaged.txt ]; then echo "unstaged kept"; else echo "unstaged lost"; fi
"#;

fn puddle_bin() -> PathBuf {
    PathBuf::from(
        std::env::var_os("CARGO_BIN_EXE_puddle").expect(
            "CARGO_BIN_EXE_puddle: run through cargo or nextest (it sets the built binary)",
        ),
    )
}

/// A program folder laid out as an install: `puddle` itself (a copy: the program finds its
/// runtime beside its own resolved path) and `runtime`, the run's msb runtime folder.
fn program_folder(root: &Path, runtime_dir: &Path) -> PathBuf {
    let folder = root.join("program");
    std::fs::create_dir_all(&folder).unwrap();
    let exe = folder.join("puddle");
    std::fs::copy(puddle_bin(), &exe)
        .expect("copy the built `puddle` program into the program folder");
    std::os::unix::fs::symlink(runtime_dir, folder.join("runtime")).unwrap();
    exe
}

/// One running `puddle serve` over `data`.
struct Serve {
    child: Child,
    api: Api,
}

impl Serve {
    async fn start(exe: &Path, data: &Path) -> Self {
        let connection = data.join("api.json");
        let _ = std::fs::remove_file(&connection);
        let child = Command::new(exe)
            .args([
                "serve",
                "--headless",
                "--secret-store",
                "file",
                "--port",
                "0",
            ])
            .arg("--data-dir")
            .arg(data)
            .arg("--connection-file")
            .arg(&connection)
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("start puddle serve");
        let pid = child.id();
        let info: Value = eventually_async(
            "puddle serve writes its connection file",
            Duration::from_secs(120),
            || async {
                assert!(
                    Path::new(&format!("/proc/{pid}")).exists(),
                    "puddle serve exited before it wrote {}",
                    connection.display()
                );
                std::fs::read(&connection)
                    .ok()
                    .and_then(|raw| serde_json::from_slice::<Value>(&raw).ok())
            },
        )
        .await;
        let url = info["url"].as_str().unwrap();
        let api = Api {
            addr: url
                .strip_prefix("http://")
                .unwrap_or_else(|| panic!("not a loopback http URL: {url}"))
                .trim_end_matches('/')
                .to_owned(),
            token: info["token"].as_str().unwrap().to_owned(),
        };
        Self { child, api }
    }

    /// Ends it the way a user's Ctrl-C does, and waits for its clean exit.
    fn stop(mut self) {
        let status = Command::new("kill")
            .args(["-TERM", &self.child.id().to_string()])
            .status()
            .unwrap();
        assert!(status.success());
        let code = self.child.wait().unwrap().code();
        assert_eq!(code, Some(0), "puddle serve did not stop cleanly");
    }
}

/// A client of the API, over plain HTTP/1.1 (the API listens on loopback).
struct Api {
    addr: String,
    token: String,
}

impl Api {
    async fn call(&self, method: &str, path: &str, body: Option<&Value>) -> (u16, Value) {
        let mut stream = TcpStream::connect(&self.addr).await.unwrap();
        let body = body.map(Value::to_string).unwrap_or_default();
        let request = format!(
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\n\
             Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            self.addr,
            self.token,
            body.len()
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).await.unwrap();
        let text = String::from_utf8_lossy(&raw).into_owned();
        let (head, rest) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
        let status = head
            .split(' ')
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(|| panic!("no status in {head:?}"));
        let rest = if head
            .to_ascii_lowercase()
            .contains("transfer-encoding: chunked")
        {
            dechunk(rest)
        } else {
            rest.to_owned()
        };
        (status, serde_json::from_str(&rest).unwrap_or(Value::Null))
    }

    async fn workspace(&self, id: &str) -> Value {
        let (status, body) = self
            .call("GET", &format!("/api/workspaces/{id}"), None)
            .await;
        assert_eq!(status, 200, "{body}");
        body
    }

    /// Waits until workspace `id` runs and is not busy; a failed operation fails the test.
    async fn until_running(&self, id: &str, within: Duration) {
        eventually_async("the workspace runs", within, || async {
            let w = self.workspace(id).await;
            let status = w["status"].as_str().unwrap_or_default().to_owned();
            assert!(
                !matches!(status.as_str(), "crashed" | "volume_missing"),
                "the workspace is {status}: {w}"
            );
            if status == "running" && w["busy"].is_null() {
                Ok(())
            } else {
                Err(format!("{status}, busy {}", w["busy"]))
            }
        })
        .await;
    }
}

fn dechunk(body: &str) -> String {
    let mut out = String::new();
    let mut rest = body;
    while let Some((size, after)) = rest.split_once("\r\n") {
        let Ok(n) = usize::from_str_radix(size.trim(), 16) else {
            break;
        };
        if n == 0 {
            break;
        }
        out.push_str(after.get(..n).unwrap_or(after));
        rest = after.get(n + 2..).unwrap_or("");
    }
    out
}

/// Runs `script` with argument `arg` in the guest of workspace `id` through the run's msb, as
/// root, the way `msb exec` reaches a sandbox outside Puddle (the repository is root's, so git's
/// ownership check never stands between the test and it).
fn in_guest(runtime_dir: &Path, data: &Path, id: &str, script: &str, arg: &str) -> String {
    let home = data.join("msb");
    let out = Command::new(runtime_dir.join("msb"))
        .args(["exec", id, "--", "sh", "-c", script, "puddle-test", arg])
        .env("MSB_HOME", &home)
        .env("MSB_CONFIG_PATH", home.join("config.json"))
        .env("MSB_PATH", runtime_dir.join("msb"))
        .stdin(Stdio::null())
        .output()
        .expect("msb exec");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "msb exec in {id}: {text}");
    text
}

/// Every process below `root` (children, their children, ...), from `/proc`.
fn descendants(root: u32) -> BTreeSet<u32> {
    let mut parent_of = Vec::new();
    for entry in std::fs::read_dir("/proc").unwrap().flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        if let Some(ppid) = stat_field(pid, 1).and_then(|p| p.parse::<u32>().ok()) {
            parent_of.push((pid, ppid));
        }
    }
    let mut found = BTreeSet::new();
    let mut todo = vec![root];
    while let Some(p) = todo.pop() {
        for &(pid, ppid) in &parent_of {
            if ppid == p && found.insert(pid) {
                todo.push(pid);
            }
        }
    }
    found
}

/// Field `n` after the command name of `/proc/<pid>/stat` (0 is the state, 1 the parent).
fn stat_field(pid: u32, n: usize) -> Option<String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (_, after) = stat.rsplit_once(") ")?;
    after.split(' ').nth(n).map(str::to_owned)
}

/// A process that is gone, or a zombie nobody has reaped yet: it runs no more code.
fn ended(pid: u32) -> bool {
    stat_field(pid, 0).is_none_or(|state| state == "Z" || state == "X")
}

/// The name of each process in `pids` that still runs.
fn still_running(pids: &BTreeSet<u32>) -> Vec<String> {
    pids.iter()
        .filter(|p| !ended(**p))
        .map(|p| {
            let cmd = std::fs::read(format!("/proc/{p}/cmdline")).unwrap_or_default();
            format!("{p} {}", String::from_utf8_lossy(&cmd).replace('\0', " "))
        })
        .collect()
}

/// Points the msb home's image cache at `PUDDLE_VM_IMAGE_CACHE` (CI restores it), as the other
/// VM tests do, so the image comes from disk after the first run.
fn share_image_cache(data: &Path) {
    if let Some(shared) = std::env::var_os("PUDDLE_VM_IMAGE_CACHE").map(PathBuf::from) {
        let home = data.join("msb");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&shared).unwrap();
        std::os::unix::fs::symlink(&shared, home.join("cache")).unwrap();
    }
}

/// Stops and deletes workspace `id` through the API, as a user does, and waits until it is gone.
async fn delete_workspace(api: &Api, id: &str) {
    let (code, body) = api
        .call("POST", &format!("/api/workspaces/{id}/stop"), None)
        .await;
    assert_eq!(code, 202, "{body}");
    eventually_async("the workspace stops", START_WAIT, || async {
        let w = api.workspace(id).await;
        w["status"] == "stopped" && w["busy"].is_null()
    })
    .await;
    let (_, check) = api
        .call("GET", &format!("/api/workspaces/{id}/delete-check"), None)
        .await;
    let (code, body) = api
        .call(
            "DELETE",
            &format!("/api/workspaces/{id}"),
            Some(&json!({"confirm": true, "fingerprint": check["fingerprint"]})),
        )
        .await;
    assert!(code == 202 || code == 200 || code == 204, "{code} {body}");
    eventually_async("the workspace is deleted", START_WAIT, || async {
        let (code, list) = api.call("GET", "/api/workspaces", None).await;
        code == 200
            && !list["workspaces"]
                .as_array()
                .is_some_and(|l| l.iter().any(|w| w["id"] == id))
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vm_serve_sigkill_keeps_the_commits_and_the_next_start_recovers_the_workspace() {
    let help = Command::new(puddle_bin()).arg("--help").output().unwrap();
    assert!(
        String::from_utf8_lossy(&help.stdout).contains("serve"),
        "the built `puddle` program has no `serve` command: build it with `--features puddle/serve` (vm-linux.yml does)"
    );
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .try_init();
    let settings = Settings::from_lookup(|var| std::env::var(var).ok()).expect("VM test settings");
    let pair = settings.prepare().expect("msb runtime pair");
    let runtime_dir = pair.msb.parent().unwrap().to_path_buf();
    let root = settings
        .claim_scratch_home("servekill", &test_owner())
        .expect("one scratch home tag per test");
    std::fs::create_dir_all(&root).unwrap();
    let exe = program_folder(&root, &runtime_dir);
    let data = root.join("data");
    share_image_cache(&data);

    // S1: a workspace with work worth losing.
    let first = Serve::start(&exe, &data).await;
    let (status, made) = first
        .api
        .call(
            "POST",
            "/api/workspaces",
            Some(&json!({"name": "qa-crash", "memory_mib": 1024})),
        )
        .await;
    assert_eq!(status, 202, "{made}");
    let id = made["id"].as_str().unwrap().to_owned();
    first.api.until_running(&id, CREATE_WAIT).await;
    let folder = puddle_workspace::Layout::new(&puddle_types::WorkspaceId::new(&id).unwrap())
        .unwrap()
        .mount()
        .to_string();
    let made_count = in_guest(&runtime_dir, &data, &id, MAKE_WORK, &folder);
    assert!(
        made_count.trim_end().ends_with(&COMMITS.to_string()),
        "{made_count}"
    );

    // S2: SIGKILL, no shutdown; everything it started is gone within the bar.
    let mut first = first;
    let serve_pid = first.child.id();
    let started = descendants(serve_pid);
    assert!(
        !started.is_empty(),
        "puddle serve runs a workspace but has no child process"
    );
    eprintln!("processes of puddle serve before the kill: {started:?}");
    let killed = Instant::now();
    first.child.kill().unwrap();
    first.child.wait().unwrap();
    eventually_async(
        "every process puddle serve started ends",
        KILL_BAR,
        || async {
            let left = still_running(&started);
            if left.is_empty() {
                Ok(())
            } else {
                Err(left.join("; "))
            }
        },
    )
    .await;
    eprintln!("all gone {:?} after the kill", killed.elapsed());

    // S3: the next start reconciles, and the work is there.
    let second = Serve::start(&exe, &data).await;
    let shown = second.api.workspace(&id).await;
    let status = shown["status"].as_str().unwrap_or_default();
    assert!(
        status == "crashed" || status == "stopped",
        "after the kill the workspace shows {status}: {shown}"
    );
    let (code, body) = second
        .api
        .call("POST", &format!("/api/workspaces/{id}/start"), None)
        .await;
    assert_eq!(code, 202, "{body}");
    second.api.until_running(&id, START_WAIT).await;
    let report = in_guest(&runtime_dir, &data, &id, READ_WORK, &folder);
    eprintln!("after the restart:\n{report}");
    assert!(report.contains(&format!("count {COMMITS}\n")), "{report}");
    assert!(report.contains("fsck clean\n"), "{report}");
    if report.contains("staged lost") {
        let (code, problems) = second.api.call("GET", "/api/problems", None).await;
        assert_eq!(code, 200, "{problems}");
        let said = problems["problems"].as_array().unwrap().iter().any(|p| {
            p["workspace"] == id.as_str()
                && p["detail"]
                    .as_str()
                    .is_some_and(|d| d.contains("no longer staged"))
        });
        assert!(
            said,
            "the staged file was lost and no problem says so: {problems}"
        );
    }

    delete_workspace(&second.api, &id).await;
    second.stop();
}
