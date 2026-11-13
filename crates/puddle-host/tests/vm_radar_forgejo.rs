// SPDX-License-Identifier: AGPL-3.0-or-later
//! Git over HTTPS against a real Forgejo started for the run (`puddle_e2e::lab::Forgejo`), on a
//! real microVM: the guest's own `git` clones, pushes a branch and deletes it through the host's
//! wiring (the workspace's CA, the injector over the host's secret store, the repository table),
//! a push to a repository that is not on the push list is refused, and the guest is searched for
//! the token.
//!
//! This is the nightly's real Git host (no outside host, account, token or secret): Forgejo, its
//! account and its token exist only for the run, so it runs on a forked pull request too. The
//! mocks (`vm_git`) check what Puddle sends; this checks what a real forge makes of it.
//!
//! Needs `PUDDLE_FORGEJO_BIN` (`ci/fetch-forgejo.sh`); without it the test skips unless
//! `PUDDLE_REQUIRE_FORGEJO` is set (`.github/workflows/lab-forgejo.yml` sets it). Forgejo has no
//! Windows release, so the Windows runner runs Gitea's Windows release (`ci/fetch-forgejo.sh <dir>
//! "" windows`) through the same fixture (`vm_radar_*` is left out of the per-pull-request VM jobs:
//! ci/vm-filter.sh; lab-forgejo.yml runs it on both).

mod support;
mod vm_support;

use std::collections::BTreeSet;
use std::time::Duration;

use puddle_e2e::lab::{FORGEJO_OWNER, Forgejo, NameProxy};
use puddle_secrets::{HostName, Secret, SecretStore, SourceSpec, StoredId, TokenScope};
use puddle_store::{
    Actor, Author, Coverage, CredentialBinding, Effect, IdentityDraft, NewRule, Owner, Pattern,
    Scope,
};
use puddle_types::WorkspaceId;
use serde_json::json;
use support::{Api, ended};
use vm_support::{
    VmHost, guest, guest_within, platform_trusting, start_vm_host_platform, through_company_proxy,
    with_fake_names,
};

const NAME: &str = "bound.invalid";

/// `"first""second"`: the shell joins them, the script text never holds the value.
fn split(value: &str) -> String {
    let (head, tail) = value.split_at(value.len() / 2);
    format!("\"{head}\"\"{tail}\"")
}

/// The guest holds no token: not in any process's environment or command line, not in any file of
/// the disk. The control comes first: a token planted in a file and in a process's environment
/// must be found, or the search proves nothing.
async fn assert_guest_holds_no_token(vm: &VmHost, sandbox: &WorkspaceId, token: &str) {
    let search = format!(
        "w={w}; printf '%s\\n' \"$w\" \"$(printf 'x-access-token:%s' \"$w\" | base64 -w0)\" \"$(printf '%s' \"$w\" | base64 -w0)\" > /dev/shm/.p; \
         echo procs=$(cat /proc/[0-9]*/environ /proc/[0-9]*/cmdline 2>/dev/null | tr '\\0' '\\n' | grep -cFf /dev/shm/.p); \
         echo files=$(grep -rlaFf /dev/shm/.p --exclude-dir=proc --exclude-dir=sys --exclude-dir=dev --exclude=.p / 2>/dev/null | grep -v '^/dev/shm/.p$' | tr '\\n' ' '); \
         rm -f /dev/shm/.p; echo done",
        w = split(token)
    );
    let planted = format!(
        "w={w}; printf '%s\\n' \"$w\" > /root/planted.txt; \
         env PLANTED=\"$w\" sleep 120 & pid=$!; sleep 1; {search}; kill $pid; rm -f /root/planted.txt",
        w = split(token)
    );
    let control = guest_within(&vm.runtime, sandbox, &planted, Duration::from_secs(600)).await;
    assert!(
        !control.contains("procs=0") && control.contains("/root/planted.txt"),
        "the search must find a planted token in a process and in a file: {control}"
    );
    let found = guest_within(&vm.runtime, sandbox, &search, Duration::from_secs(600)).await;
    assert!(
        found.contains("procs=0"),
        "a token is in a process: {found}"
    );
    assert!(
        found.contains("files= done") || found.contains("files=\ndone"),
        "a token is on the guest's disk: {found}"
    );
}

/// Clone, push a branch, delete it, a refused push to an unlisted repository, and the token
/// search, against a real Forgejo.
#[expect(clippy::too_many_lines, reason = "one story, read top to bottom")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_radar_forgejo_clones_pushes_and_deletes_a_branch_and_the_guest_never_holds_the_token() {
    let Some(forgejo) = Forgejo::start(NAME) else {
        return;
    };
    forgejo.add_repo("acme/app", true);
    forgejo.add_repo("acme/other", true);
    let token = forgejo.token().to_owned();

    let rt = tokio::runtime::Handle::current();
    let proxy =
        tokio::task::block_in_place(|| rt.block_on(NameProxy::start(&[(NAME, forgejo.addr())])));
    let options = with_fake_names(
        through_company_proxy(support::named_id_options(), &proxy),
        [NAME],
    );
    let platform = platform_trusting([forgejo.root()]);
    let vm = start_vm_host_platform("fgj", options, &platform, |_| {}).await;
    let name = format!("{}-fgj", vm.settings.prefix.as_str());
    let sandbox = WorkspaceId::new(&name).unwrap();
    vm.host
        .store()
        .add_rule(&NewRule {
            scope: Scope::Workspace(sandbox.clone()),
            pattern: Pattern::parse(NAME).unwrap(),
            effect: Effect::Allow,
            expires_at: None,
            created_by: Actor::Cli,
        })
        .unwrap();
    // The one credential: Forgejo's token for the run, in the host's secret store, covering the
    // owner's repositories.
    let host = HostName::new(NAME).unwrap();
    let id = StoredId::new("fgjwork").unwrap();
    vm.secrets.set(&id, &Secret::new(token.clone())).unwrap();
    let binding = CredentialBinding::new(
        &host,
        SourceSpec::Stored {
            id,
            scope: TokenScope {
                host: host.clone(),
                org: None,
            },
        },
        Coverage::new(BTreeSet::from([Owner::new(FORGEJO_OWNER).unwrap()]), false).unwrap(),
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

    // The workspace is created by cloning the private repository: its first request carries the
    // token, which Forgejo checks.
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
    assert_eq!(
        end.get("step"),
        Some(&json!("done")),
        "{end}\n{}",
        forgejo.log()
    );

    // The guest's own git: clone, push a branch, see it on the server.
    let script = "set -eu; rm -rf /tmp/f; mkdir /tmp/f; cd /tmp/f; \
         git clone -q https://bound.invalid/acme/app.git app; cd app; \
         git checkout -q -b guest-branch; echo hi > pushed.txt; git add pushed.txt; \
         git commit -q -m guest; git push -q origin guest-branch; \
         git log -1 --format=%ae; git rev-parse HEAD";
    let out = guest(&vm.runtime, &sandbox, script).await;
    let mut lines = out.lines();
    let author = lines.next().unwrap();
    let pushed = lines.next().unwrap();
    assert_eq!(author, "work@example.org", "{out}");
    assert_eq!(
        forgejo.branch_head("acme/app", "guest-branch").as_deref(),
        Some(pushed),
        "the push reached Forgejo\n{}",
        forgejo.log()
    );

    // Delete the branch from the guest.
    guest(
        &vm.runtime,
        &sandbox,
        "set -eu; cd /tmp/f/app; git push -q origin --delete guest-branch",
    )
    .await;
    assert_eq!(
        forgejo.branch_head("acme/app", "guest-branch"),
        None,
        "the branch is gone from Forgejo"
    );

    // A push to a repository that is not on the push list: refused in git's own output, and the
    // repository on the server is unchanged.
    let other_head = forgejo.branch_head("acme/other", "main").unwrap();
    let out = guest(
        &vm.runtime,
        &sandbox,
        "cd /tmp/f; git clone -q https://bound.invalid/acme/other.git other; cd other; \
         echo x > x.txt; git add x.txt; git commit -q -m x; \
         git push origin HEAD:main 2>&1 || true",
    )
    .await;
    assert!(out.contains("not on this workspace's push list"), "{out}");
    assert_eq!(
        forgejo.branch_head("acme/other", "main").unwrap(),
        other_head,
        "the refused push changed nothing on Forgejo"
    );

    assert_guest_holds_no_token(&vm, &sandbox, &token).await;

    // The audit log holds no token either, and its rows say the connections were injected.
    let audit = api
        .get(&format!(
            "/api/audit?workspace={name}&type=connection&limit=200"
        ))
        .await;
    assert!(
        !audit.body.contains(&token),
        "a token reached the audit log"
    );
    assert!(audit.body.contains("\"injected\":true"), "{}", audit.body);

    let _ = proxy.connects();
    let down = vm.host.shutdown().await;
    assert!(down.sandboxes.all_stopped(), "{down:?}");
}
