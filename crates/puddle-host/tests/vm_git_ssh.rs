// SPDX-License-Identifier: AGPL-3.0-or-later
//! Git over SSH end to end on real microVMs: the guest's own OpenSSH, `git` and cargo's
//! built-in Git (libgit2) talk SSH to a Git host Puddle knows, Puddle ends the SSH session itself
//! and reaches the host the way the workspace's identity does, so the guest holds no key, no agent
//! socket and no token, and the repository table decides every push as it does over HTTPS.
//!
//! The Git host is the lab's (`puddle_e2e::lab::GitHost`: the real `git http-backend` behind TLS,
//! reachable only through a company proxy that alone knows its name), so what reaches it is what
//! Puddle sent on the guest's behalf.
#![expect(
    clippy::unwrap_used,
    reason = "VM test: a failed step fails the test; measurements go to stderr"
)]

mod support;
mod vm_support;

use std::collections::BTreeSet;
use std::path::Path;
use std::time::{Duration, Instant};

use base64::Engine as _;
use puddle_e2e::lab::{GitHost, NameProxy, Versions, clean_git};
use puddle_host::HostOptions;
use puddle_secrets::{HostName, Secret, SecretStore, SourceSpec, StoredId, TokenScope};
use puddle_store::{
    Actor, Author, Coverage, CredentialBinding, Effect, IdentityDraft, NewRule, Owner, Pattern,
    Scope,
};
use puddle_types::WorkspaceId;
use serde_json::json;
use support::{Api, ended};
use vm_support::{
    HarnessPlatform, VmHost, guest, guest_within, platform_trusting, start_vm_host_platform,
    through_company_proxy, with_fake_names,
};

const NAME: &str = "bound.invalid";
const WORK: &str = "SCANARY-WORK-5b1e8f20c4d9";

struct Internet {
    proxy: NameProxy,
    options: HostOptions,
    platform: HarnessPlatform,
}

fn internet(host: &GitHost) -> Internet {
    let rt = tokio::runtime::Handle::current();
    let proxy =
        tokio::task::block_in_place(|| rt.block_on(NameProxy::start(&[(NAME, host.addr())])));
    let options = with_fake_names(
        through_company_proxy(support::named_id_options(), &proxy),
        [NAME],
    );
    let platform = platform_trusting([host.root()]);
    Internet {
        proxy,
        options,
        platform,
    }
}

/// The fake host: `acme/app` (the workspace's repository), `acme/other` (readable, not on the
/// push list), `acme/dep` (a crate for cargo), all private.
async fn git_host() -> GitHost {
    let host = GitHost::start(NAME, Versions::Both).await;
    host.accept_token(WORK);
    for repo in ["acme/app", "acme/other", "acme/dep"] {
        host.add_repo(repo, true);
    }
    host
}

/// A workspace `<prefix>-<tag>` cloned from `acme/app` over HTTPS, with one identity, Work, whose
/// stored token covers the `acme` owner; `bound.invalid` is allowed.
async fn workspace(fake: &GitHost, tag: &str) -> (NameProxy, VmHost, WorkspaceId) {
    let net = internet(fake);
    let vm = start_vm_host_platform(tag, net.options, &net.platform, |_| {}).await;
    let name = format!("{}-{tag}", vm.settings.prefix.as_str());
    let sandbox = WorkspaceId::new(&name).unwrap();
    vm.host
        .store()
        .as_user(puddle_store::UserId::OWNER)
        .add_rule(&NewRule {
            scope: Scope::Workspace(sandbox.clone()),
            pattern: Pattern::parse(NAME).unwrap(),
            effect: Effect::Allow,
            expires_at: None,
            created_by: Actor::Cli,
        })
        .unwrap();
    let host = HostName::new(NAME).unwrap();
    let id = StoredId::new(format!("{tag}work")).unwrap();
    vm.secrets.set(&id, &Secret::new(WORK.to_owned())).unwrap();
    let binding = CredentialBinding::new(
        &host,
        SourceSpec::Stored {
            id,
            scope: TokenScope {
                host: host.clone(),
                org: None,
            },
        },
        Coverage::new(BTreeSet::from([Owner::new("acme").unwrap()]), false).unwrap(),
    )
    .unwrap();
    vm.host
        .store()
        .create_identity(
            puddle_store::UserId::OWNER,
            IdentityDraft {
                label: "Work".to_owned(),
                author: Author::new("Work", "work@example.org").unwrap(),
                credentials: vec![binding],
            },
        )
        .unwrap();
    let api = Api::new(vm.host.url(), vm.host.token());
    let mut events = api.events().await;
    let name_static: &'static str = Box::leak(name.clone().into_boxed_str());
    let reply = api
        .post(
            "/api/workspaces",
            &json!({"name": name, "repositories": [{"url": format!("https://{NAME}/acme/app.git")}], "memory_mib": 2048})
                .to_string(),
        )
        .await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    let end = events
        .until(ended(name_static), Duration::from_secs(600))
        .await;
    assert_eq!(end.get("step"), Some(&json!("done")), "{end}");
    (net.proxy, vm, sandbox)
}

/// Runs `git` on the host against the bare repository `repo` of `host` through a scratch clone.
fn on_host(host: &GitHost, repo: &str, script: &str) -> String {
    let work = host.scratch().join(format!("w-{}", repo.replace('/', "-")));
    let _ = std::fs::remove_dir_all(&work);
    let mut clone = clean_git(host.scratch());
    let out = clone
        .arg("clone")
        .arg("-q")
        .arg(host.repo_path(repo))
        .arg(&work)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = std::process::Command::new("sh")
        .arg("-c")
        .arg(script)
        .current_dir(&work)
        .env("HOME", host.scratch())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", host.scratch().join("gitconfig"))
        .env("GIT_AUTHOR_NAME", "host")
        .env("GIT_AUTHOR_EMAIL", "host@example.org")
        .env("GIT_COMMITTER_NAME", "host")
        .env("GIT_COMMITTER_EMAIL", "host@example.org")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{script}\n{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn head_of(host: &GitHost, repo: &str) -> String {
    let out = clean_git(host.scratch())
        .arg("--git-dir")
        .arg(host.repo_path(repo))
        .args(["rev-parse", "refs/heads/main"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn basic(token: &str) -> String {
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("x-access-token:{token}"))
    )
}

/// `"first""second"`: the shell joins them, the script text never holds the value.
fn split(value: &str) -> String {
    let (head, tail) = value.split_at(value.len() / 2);
    format!("\"{head}\"\"{tail}\"")
}

/// Runs `script` in the guest and returns its exit status and output, whatever the status.
async fn guest_status(vm: &VmHost, sandbox: &WorkspaceId, script: &str) -> (i32, String) {
    let wrapped = format!("( {script} ) 2>&1; echo \"exit=$?\"");
    let out = guest_within(&vm.runtime, sandbox, &wrapped, Duration::from_secs(300)).await;
    let (body, code) = out.trim_end().rsplit_once("exit=").unwrap();
    (code.trim().parse().unwrap(), body.to_owned())
}

/// The guest holds no token, no private key Puddle's SSH server uses and no agent socket.
async fn assert_guest_holds_no_secret(vm: &VmHost, sandbox: &WorkspaceId) {
    let search = format!(
        "w={work}; printf '%s\\n' \"$w\" \"$(printf 'x-access-token:%s' \"$w\" | base64 -w0)\" > /dev/shm/.p; \
         echo procs=$(cat /proc/[0-9]*/environ /proc/[0-9]*/cmdline 2>/dev/null | tr '\\0' '\\n' | grep -cFf /dev/shm/.p); \
         echo files=$(grep -rlaFf /dev/shm/.p --exclude-dir=proc --exclude-dir=sys --exclude-dir=dev / 2>/dev/null | grep -v '^/dev/shm/.p$' | tr '\\n' ' '); \
         rm -f /dev/shm/.p; \
         echo agent=$(cat /proc/[0-9]*/environ 2>/dev/null | tr '\\0' '\\n' | grep -c '^SSH_AUTH_SOCK='); \
         grep -v '^#' /etc/ssh/ssh_known_hosts | awk '{{print $2\" \"$3}}' | sort -u > /dev/shm/.k; \
         for f in $(grep -rlI -e 'PRIVATE KEY' --exclude-dir=proc --exclude-dir=sys --exclude-dir=dev / 2>/dev/null); do \
           pub=$(ssh-keygen -y -f \"$f\" 2>/dev/null | awk '{{print $1\" \"$2}}'); \
           [ -n \"$pub\" ] && grep -qxF \"$pub\" /dev/shm/.k && echo \"HOST KEY: $f\"; \
         done; rm -f /dev/shm/.k; echo done",
        work = split(WORK),
    );
    let found = guest_within(&vm.runtime, sandbox, &search, Duration::from_secs(600)).await;
    assert!(
        found.contains("procs=0"),
        "a token is in a process: {found}"
    );
    assert!(
        found.contains("files= ") || found.contains("files=\n"),
        "a token is on the guest's disk: {found}"
    );
    assert!(found.contains("agent=0"), "an agent socket is set: {found}");
    assert!(!found.contains("HOST KEY"), "{found}");
}

/// The `entries` of the workspace's connection records for the Git host.
async fn audit(vm: &VmHost, sandbox: &WorkspaceId) -> Vec<serde_json::Value> {
    let api = Api::new(vm.host.url(), vm.host.token());
    let page = api
        .get(&format!(
            "/api/audit?workspace={}&type=connection&host_contains={NAME}&limit=500",
            sandbox.as_str()
        ))
        .await;
    assert!(!page.body.contains(WORK), "a token reached the audit log");
    page.json()
        .get("entries")
        .unwrap()
        .as_array()
        .unwrap()
        .clone()
}

/// The guest's own `ssh` and `git` over SSH: clone, fetch after a commit on the host, push to the
/// listed repository, `ls-remote`, all on protocol version 2; a push to an unlisted repository is
/// refused in the HTTPS words and never reaches the host; one connection record names the push;
/// a user known-hosts file with another key for the name still connects; `ssh-keyscan` returns
/// the keys of the system known-hosts file; `ssh -T` gets the greeting; and the guest holds no
/// key, no agent socket and no token.
#[expect(clippy::too_many_lines, reason = "one story, read top to bottom")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_git_ssh_clones_fetches_and_pushes_over_ssh_and_the_guest_holds_no_key() {
    let fake = git_host().await;
    let (net, vm, sandbox) = workspace(&fake, "sshk").await;

    let out = guest(
        &vm.runtime,
        &sandbox,
        "set -eu; rm -rf /tmp/s; mkdir /tmp/s; cd /tmp/s; \
         GIT_TRACE_PACKET=/tmp/s/clone-trace.txt git clone -q git@bound.invalid:acme/app.git app; \
         git -C app rev-parse HEAD",
    )
    .await;
    assert_eq!(
        out.trim(),
        head_of(&fake, "acme/app"),
        "the clone has the host's head"
    );
    let trace = guest(
        &vm.runtime,
        &sandbox,
        "grep -c 'version 2' /tmp/s/clone-trace.txt || true",
    )
    .await;
    assert_ne!(trace.trim(), "0", "the clone spoke protocol version 2");

    // A commit on the host, then a fetch in the guest.
    let server = on_host(
        &fake,
        "acme/app",
        "echo server > server.txt && git add server.txt && git commit -q -m server && git push -q origin HEAD:main && git rev-parse HEAD",
    );
    let fetched = guest(
        &vm.runtime,
        &sandbox,
        "set -eu; cd /tmp/s/app; git fetch -q; git rev-parse origin/main",
    )
    .await;
    assert_eq!(
        fetched.trim(),
        server,
        "the fetch brought the host's new commit"
    );

    // A push to the listed repository lands.
    let pushed = guest(
        &vm.runtime,
        &sandbox,
        "set -eu; cd /tmp/s/app; git merge -q --ff-only origin/main; echo guest > guest.txt; \
         git add guest.txt; git commit -q -m guest; git push -q origin HEAD:main; git rev-parse HEAD",
    )
    .await;
    assert_eq!(
        head_of(&fake, "acme/app"),
        pushed.trim(),
        "the push reached the host"
    );
    let refs = guest(
        &vm.runtime,
        &sandbox,
        "git ls-remote git@bound.invalid:acme/app.git",
    )
    .await;
    assert!(
        refs.contains(&format!("{}\trefs/heads/main", pushed.trim())),
        "{refs}"
    );

    // A push to a repository that is not on the push list: refused with the HTTPS words, and the
    // host hears nothing about a push.
    let before = fake.seen().len();
    let (code, out) = guest_status(
        &vm,
        &sandbox,
        "cd /tmp/s && git clone -q git@bound.invalid:acme/other.git other && cd other && \
         echo x > x.txt && git add x.txt && git commit -q -m x && git push origin HEAD:main",
    )
    .await;
    assert_eq!(code, 128, "{out}");
    assert!(
        out.contains(
            "puddle: push to bound.invalid/acme/other is not on this workspace's push list"
        ),
        "{out}"
    );
    assert!(
        fake.seen()
            .iter()
            .skip(before)
            .all(|r| !r.target.contains("receive-pack")),
        "no push request reached the host"
    );

    // The audit: one record for the push, with its service, repository and credential.
    let entries = audit(&vm, &sandbox).await;
    let push = entries
        .iter()
        .find(|e| e.to_string().contains("git-receive-pack") && e.to_string().contains("acme/app"))
        .unwrap_or_else(|| panic!("no record of the SSH push: {entries:#?}"));
    assert_eq!(push["record"]["injected"], json!(true), "{push}");
    assert!(push["record"]["binding_id"].is_string(), "{push}");

    // The host saw the token Puddle added on every request, and nothing else.
    for request in fake.seen() {
        assert_eq!(
            request.headers_named("authorization"),
            [basic(WORK)],
            "{}",
            request.target
        );
    }

    // A user known-hosts file with another key for the name: OpenSSH accepts any listed key.
    let out = guest(
        &vm.runtime,
        &sandbox,
        "set -eu; mkdir -p ~/.ssh; rm -f /tmp/s/okey /tmp/s/okey.pub; \
         ssh-keygen -q -t ed25519 -N '' -f /tmp/s/okey; \
         echo \"bound.invalid $(cut -d' ' -f1,2 /tmp/s/okey.pub)\" >> ~/.ssh/known_hosts; \
         rm -f /tmp/s/okey; git ls-remote git@bound.invalid:acme/app.git | grep -c refs/heads/main",
    )
    .await;
    assert_eq!(out.trim(), "1", "{out}");

    // ssh-keyscan reaches Puddle's server and gets the keys of the system file.
    let out = guest(
        &vm.runtime,
        &sandbox,
        "ssh-keyscan bound.invalid 2>/dev/null | grep -v '^#' | awk '{print $2\" \"$3}' | sort -u > /tmp/s/scan; \
         n=$(wc -l < /tmp/s/scan); missing=0; \
         while read -r k; do grep -qF \"$k\" /etc/ssh/ssh_known_hosts || { missing=$((missing+1)); echo \"not listed: $k\"; }; done < /tmp/s/scan; \
         echo scanned=$n missing=$missing",
    )
    .await;
    assert!(
        out.contains("missing=0") && !out.contains("scanned=0"),
        "{out}"
    );

    // ssh -T: the greeting, with the host's exit status.
    let (code, out) = guest_status(&vm, &sandbox, "ssh -T git@bound.invalid").await;
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("successfully authenticated"), "{out}");

    assert_guest_holds_no_secret(&vm, &sandbox).await;
    let _ = net.connects();
    let down = vm.host.shutdown().await;
    assert!(down.sandboxes.all_stopped(), "{down:?}");
}

/// The guest is Linux, so the host's own cargo is a guest program only on a Linux host (a Windows
/// or macOS host has a PE or Mach-O file there); the libgit2 step is guest-side behaviour and the
/// Linux tier covers it.
const CARGO_RUNS_IN_GUEST: bool = cfg!(target_os = "linux");

/// The cargo binary of the toolchain running this test (cargo sets `CARGO` for what it runs).
fn host_cargo() -> std::path::PathBuf {
    let cargo = std::env::var_os("CARGO").map_or_else(
        || {
            let out = std::process::Command::new("rustup")
                .args(["which", "cargo"])
                .output()
                .unwrap();
            std::path::PathBuf::from(String::from_utf8_lossy(&out.stdout).trim())
        },
        std::path::PathBuf::from,
    );
    assert!(cargo.is_file(), "{}", cargo.display());
    cargo
}

/// Puts `file` into the public repository `tools/bin` of `host` as `name`, so the guest can fetch
/// it over HTTPS (a tool the image lacks, from the host's own toolchain).
fn publish_tool(host: &GitHost, file: &Path, name: &str) {
    if !host.repo_path("tools/bin").exists() {
        host.add_repo("tools/bin", false);
    }
    let target = host.scratch().join(name);
    std::fs::copy(file, &target).unwrap();
    on_host(
        host,
        "tools/bin",
        &format!(
            "cp '{}' ./{name} && git add --chmod=+x {name} && git commit -q -m {name} && git push -q origin HEAD:main",
            target.display()
        ),
    );
}

/// Other clients reach the same terminated session: a protocol-v0 `git fetch`; cargo's built-in
/// Git (libgit2, which ignores `ssh_config` and reaches Puddle through the transparent capture);
/// and a fetch through a `ControlMaster` connection. (The guest image has no Git LFS; its SSH
/// commands are the guest side's, tested in the proxy's suite.)
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_git_ssh_other_clients_reach_the_same_session() {
    let fake = git_host().await;
    on_host(
        &fake,
        "acme/dep",
        "printf '[package]\\nname = \"dep\"\\nversion = \"0.1.0\"\\nedition = \"2021\"\\n' > Cargo.toml && \
         mkdir -p src && echo 'pub fn dep() {}' > src/lib.rs && git add . && git commit -q -m crate && git push -q origin HEAD:main",
    );
    if CARGO_RUNS_IN_GUEST {
        publish_tool(&fake, &host_cargo(), "cargo");
    }
    let (net, vm, sandbox) = workspace(&fake, "ssho").await;

    // Protocol v0: clone and fetch, and the trace never says version 2.
    let server = on_host(
        &fake,
        "acme/app",
        "echo v0 > v0.txt && git add v0.txt && git commit -q -m v0 && git push -q origin HEAD:main && git rev-parse HEAD~1",
    );
    let _ = server;
    let out = guest(
        &vm.runtime,
        &sandbox,
        "set -eu; rm -rf /tmp/o; mkdir /tmp/o; cd /tmp/o; \
         GIT_TRACE_PACKET=/tmp/o/v0.txt git -c protocol.version=0 clone -q git@bound.invalid:acme/app.git app; \
         git -C app reset -q --hard HEAD~1; git -C app update-ref refs/remotes/origin/main HEAD; \
         GIT_TRACE_PACKET=/tmp/o/v0f.txt git -C app -c protocol.version=0 fetch -q; \
         git -C app rev-parse origin/main; echo v2=$(cat /tmp/o/v0.txt /tmp/o/v0f.txt | grep -c 'version 2')",
    )
    .await;
    assert!(out.starts_with(&head_of(&fake, "acme/app")), "{out}");
    assert!(
        out.contains("v2=0"),
        "the v0 run never spoke version 2: {out}"
    );
    // A v0 fetch with history on both sides: several negotiation rounds.
    on_host(
        &fake,
        "acme/app",
        "for i in $(seq 1 40); do echo $i > n.txt; git add n.txt; git commit -q -m n$i; done; git push -q origin HEAD:main",
    );
    let out = guest(
        &vm.runtime,
        &sandbox,
        "set -eu; cd /tmp/o/app; for i in $(seq 1 40); do echo g$i > g.txt; git add g.txt; git commit -q -m g$i; done; \
         git -c protocol.version=0 fetch -q; git rev-parse origin/main",
    )
    .await;
    assert_eq!(out.trim(), head_of(&fake, "acme/app"), "{out}");

    if CARGO_RUNS_IN_GUEST {
        // cargo's built-in Git (libgit2 over libssh2): the image's setting says use the git CLI, so
        // this turns it off, as a user may. libgit2 takes an accepted `none` as no way in and cargo
        // signs only with an agent, so the user has an agent with a key of their own (never used).
        let out = guest_within(
            &vm.runtime,
            &sandbox,
            &format!(
                "set -eu; cd /tmp/o; git clone -q https://{NAME}/tools/bin.git tools; \
                 mkdir -p user/src; cd user; echo 'fn main() {{}}' > src/main.rs; \
                 printf '[package]\\nname = \"user\"\\nversion = \"0.1.0\"\\nedition = \"2021\"\\n[dependencies]\\ndep = {{ git = \"ssh://git@{NAME}/acme/dep.git\" }}\\n' > Cargo.toml; \
                 eval $(ssh-agent -s) >/dev/null; ssh-keygen -q -t ed25519 -N '' -f /tmp/o/any; ssh-add -q /tmp/o/any; rm -f /tmp/o/any; \
                 CARGO_NET_GIT_FETCH_WITH_CLI=false CARGO_HOME=/tmp/o/cargo-home /tmp/o/tools/cargo generate-lockfile 2>&1; kill $SSH_AGENT_PID; \
                 grep -c 'ssh://git@{NAME}/acme/dep.git' Cargo.lock"
            ),
            Duration::from_secs(300),
        )
        .await;
        assert!(
            out.trim_end().ends_with('1'),
            "cargo's libgit2 fetched the SSH dependency: {out}"
        );
    }

    // ControlMaster: one master connection, two fetches through it.
    let out = guest(
        &vm.runtime,
        &sandbox,
        "set -eu; cd /tmp/o/app; \
         ssh -o ControlMaster=yes -o ControlPath=/tmp/o/cm -o ControlPersist=120 -fN git@bound.invalid; \
         export GIT_SSH_COMMAND='ssh -o ControlMaster=no -o ControlPath=/tmp/o/cm'; \
         git fetch -q; git fetch -q; git ls-remote origin | grep -c refs/heads/main; \
         ssh -o ControlPath=/tmp/o/cm -O check git@bound.invalid 2>&1",
    )
    .await;
    assert!(out.contains("Master running"), "{out}");

    let _ = net.connects();
    let down = vm.host.shutdown().await;
    assert!(down.sandboxes.all_stopped(), "{down:?}");
}

/// A hostile guest gets only the named repository: every other command form, a forward to the
/// host's ports, an agent forward, `sftp` and a terminal are refused with a reason, and the host
/// sees requests only for the repository a valid command named.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_git_ssh_a_hostile_guest_gets_only_the_named_repository() {
    let fake = git_host().await;
    let (net, vm, sandbox) = workspace(&fake, "sshh").await;
    let before = fake.seen().len();
    let commands = [
        "git-upload-archive 'acme/app.git'",
        "git upload-pack 'acme/app.git'",
        "git-upload-pack 'acme/app.git'; id",
        "git-upload-pack 'acme/app.git' && id",
        "git-upload-pack 'acme/app.git' 'acme/other.git'",
        "git-upload-pack 'acme/'\\''other.git'",
        "git-upload-pack \"acme/other.git\"",
        "git-upload-pack 'acme/../acme/other.git'",
        "git-upload-pack 'acme//other.git'",
        "git-upload-pack 'acme%2Fother.git'",
        "git-upload-pack '~acme/other.git'",
        "git-upload-pack 'acme/other.git.git'",
        "git-receive-pack 'acme/other.git'",
        "sh -c id",
    ];
    for command in commands {
        let (code, out) = guest_status(
            &vm,
            &sandbox,
            &format!("ssh git@bound.invalid {} </dev/null", shell_quote(command)),
        )
        .await;
        assert_ne!(code, 0, "{command}: {out}");
        assert!(out.contains("puddle: "), "{command}: {out}");
        assert!(!out.contains("uid="), "{command}: {out}");
    }
    let (code, out) = guest_status(
        &vm,
        &sandbox,
        "ssh -o ExitOnForwardFailure=yes -W 127.0.0.1:443 git@bound.invalid </dev/null",
    )
    .await;
    assert_ne!(code, 0, "{out}");
    let (code, out) = guest_status(&vm, &sandbox, "echo quit | sftp -b - git@bound.invalid").await;
    assert_ne!(code, 0, "{out}");
    let (_, out) = guest_status(&vm, &sandbox, "ssh -tt git@bound.invalid </dev/null").await;
    assert!(out.contains("PTY allocation request failed"), "{out}");
    let (code, out) = guest_status(
        &vm,
        &sandbox,
        "eval $(ssh-agent -s) >/dev/null; ssh-add -l >/dev/null 2>&1; \
         ssh -A git@bound.invalid \"git-upload-pack 'acme/app.git'\" </dev/null | head -c 4; kill $SSH_AGENT_PID",
    )
    .await;
    assert_eq!(code, 0, "{out}");
    for request in fake.seen().iter().skip(before) {
        assert!(
            request.target.starts_with("/acme/app.git/"),
            "{}",
            request.target
        );
    }
    let _ = net.connects();
    let down = vm.host.shutdown().await;
    assert!(down.sandboxes.all_stopped(), "{down:?}");
}

fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// Bar: a 50 MB repository's SSH clone stays within 1.2 times the HTTPS clone through Puddle
/// (median of five each, alternating, same VM, same lab).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_git_ssh_a_clone_is_within_1_2_times_https() {
    let fake = git_host().await;
    fake.add_repo("acme/big", true);
    on_host(
        &fake,
        "acme/big",
        "for i in $(seq 1 50); do head -c 1048576 /dev/urandom > f$i.bin; done; git add . && git commit -q -m big && git push -q origin HEAD:main",
    );
    let (net, vm, sandbox) = workspace(&fake, "sshb").await;
    let mut https = Vec::new();
    let mut ssh = Vec::new();
    for round in 0..6 {
        for (url, times) in [
            (format!("https://{NAME}/acme/big.git"), &mut https),
            (format!("git@{NAME}:acme/big.git"), &mut ssh),
        ] {
            let script = format!(
                "set -eu; rm -rf /tmp/b; s=$(date +%s%N); git clone -q {url} /tmp/b; e=$(date +%s%N); \
                 du -sm /tmp/b/.git | cut -f1; echo $(( (e - s) / 1000000 ))"
            );
            let started = Instant::now();
            let out = guest_within(&vm.runtime, &sandbox, &script, Duration::from_secs(600)).await;
            let mut lines = out.lines();
            let size: u64 = lines.next().unwrap().trim().parse().unwrap();
            let ms: u64 = lines.next().unwrap().trim().parse().unwrap();
            eprintln!(
                "round {round} {url}: {ms} ms in the guest ({} ms with exec), .git {size} MB",
                started.elapsed().as_millis()
            );
            assert!(size >= 50, "the clone holds the data: {size} MB");
            // Round 0 warms both paths; it is not counted.
            if round > 0 {
                times.push(ms);
            }
        }
    }
    https.sort_unstable();
    ssh.sort_unstable();
    let (h, s) = (https[2], ssh[2]);
    eprintln!(
        "https {https:?} median {h} ms; ssh {ssh:?} median {s} ms; ratio {:.3}",
        f64::from(u32::try_from(s).unwrap()) / f64::from(u32::try_from(h).unwrap())
    );
    assert!(
        s * 10 <= h * 12,
        "the SSH clone's median {s} ms is over 1.2 x the HTTPS clone's {h} ms"
    );
    let _ = net.connects();
    let down = vm.host.shutdown().await;
    assert!(down.sandboxes.all_stopped(), "{down:?}");
}
