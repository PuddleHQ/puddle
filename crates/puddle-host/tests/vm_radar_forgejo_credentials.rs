// SPDX-License-Identifier: AGPL-3.0-or-later
//! A workspace's Git credential over a working day, against a real Forgejo started for the run
//! (`puddle_e2e::lab::Forgejo`) on a real microVM, with every change made through the API while the
//! workspace runs: none of them needs a stop or a start.
//!
//! In order: a workspace made from a private repository that no identity covers gets a problem
//! holding git's own reason and sends no credential (an identity for another owner on the same host
//! holds a token the forge would take, so sending it would make the clone work); the guest's `curl`
//! trusts the forge without `-k`; an identity added to the running workspace clones; five pushes
//! land and give five injected connection records; a non-fast-forward push shows git's own
//! refusal; a push to a repository off the push list is refused, and the same guest's next push
//! lands once push is turned on for it; a token revoked on the forge makes the next fetch a `403`
//! naming the identity and raises one sign-in notice; a new token pasted into the identity is used
//! by the next fetch and push, the old one leaves the credential store, and the guest holds
//! neither.
//!
//! Needs `PUDDLE_FORGEJO_BIN` as `vm_radar_forgejo.rs` does (`.github/workflows/lab-forgejo.yml`
//! runs both on Linux and on Windows).
#![expect(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::panic,
    reason = "VM test: JSON read from the API in helpers outside the test; a missing key fails it"
)]

use puddle_host_test_support::support;
use puddle_host_test_support::vm_support;

use std::time::{Duration, Instant};

use puddle_e2e::lab::{FORGEJO_OWNER, Forgejo, NameProxy};
use puddle_secrets::{SecretStore, StoredId};
use puddle_store::{Actor, Effect, NewRule, Pattern, Scope};
use puddle_types::WorkspaceId;
use serde_json::{Value, json};
use support::{Api, Events, ended};
use vm_support::{
    VmHost, assert_guest_holds_no_token, guest, platform_trusting, start_vm_host_platform,
    through_company_proxy, with_fake_names,
};

const NAME: &str = "bound.invalid";

/// Keeps `token` for the forge in the credential store, as the Identities page's "Paste a token"
/// does, and returns the source that names it.
async fn keep_token(api: &Api, token: &str) -> Value {
    let reply = api
        .post(
            "/api/credentials/stored",
            &json!({"host": NAME, "token": token}).to_string(),
        )
        .await;
    assert_eq!(reply.status, 201, "{}", reply.body);
    reply.json()["source"].clone()
}

/// The body that makes or replaces an identity `label` whose one credential, `source`, covers the
/// forge's `owner`.
fn identity(label: &str, source: &Value, owner: &str) -> String {
    json!({
        "label": label,
        "author": {"name": label, "email": format!("{}@example.org", label.to_lowercase())},
        "credentials": [{
            "host": NAME,
            "source": source,
            "covers": {"owners": [owner], "rest_of_host": false},
        }],
    })
    .to_string()
}

/// The newest audit record of `workspace` so far, as the `after` of a later read.
async fn audit_mark(api: &Api, workspace: &str) -> i64 {
    let page = api
        .get(&format!(
            "/api/audit?workspace={workspace}&type=connection&limit=1"
        ))
        .await
        .json();
    page["next_after"].as_i64().unwrap()
}

/// The connection records of `workspace` to the forge after `mark`, oldest first, once `enough`
/// holds for them (the proxy writes a record when its connection ends).
async fn forge_connections(
    api: &Api,
    workspace: &str,
    mark: i64,
    enough: impl Fn(&[Value]) -> bool,
) -> Vec<Value> {
    let path = format!("/api/audit?workspace={workspace}&type=connection&after={mark}&limit=200");
    puddle_test_kit::eventually_async(
        "the forge's connection records",
        Duration::from_secs(60),
        || async {
            let page = api.get(&path).await.json();
            let rows: Vec<Value> = page["entries"]
                .as_array()
                .unwrap()
                .iter()
                .map(|entry| entry["record"].clone())
                .filter(|record| record["host"] == NAME)
                .collect();
            if enough(&rows) {
                Ok(rows)
            } else {
                Err(format!("{rows:#?}"))
            }
        },
    )
    .await
}

/// How many events that `pred` holds for arrive within `window`, those already waiting in the
/// stream included.
async fn count_within(
    events: &mut Events,
    window: Duration,
    pred: impl Fn(&Value) -> bool,
) -> usize {
    let deadline = Instant::now() + window;
    let mut count = 0;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        // `until` fails the test at its own limit; the shorter `timeout` ends the count first.
        let next = events.until(&pred, left + Duration::from_secs(60));
        match tokio::time::timeout(left, next).await {
            Ok(_) => count += 1,
            Err(_) => return count,
        }
    }
}

/// The commit a guest script printed as `head=<sha>`: git and the forge's `remote:` hints share
/// its output.
fn head_of(out: &str) -> &str {
    out.lines()
        .find_map(|line| line.strip_prefix("head="))
        .unwrap_or_else(|| panic!("no head= line in {out}"))
}

/// The guest's boot id: the same value at the end means the workspace never stopped.
async fn boot_id(vm: &VmHost, sandbox: &WorkspaceId) -> String {
    guest(&vm.runtime, sandbox, "cat /proc/sys/kernel/random/boot_id")
        .await
        .trim()
        .to_owned()
}

#[expect(clippy::too_many_lines, reason = "one working day, read top to bottom")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_radar_forgejo_credential_changes_apply_to_the_running_workspace_and_the_guest_never_holds_a_token()
 {
    let Some(forgejo) = Forgejo::start(NAME) else {
        return;
    };
    forgejo.add_repo("acme/app", true);
    forgejo.add_repo("acme/other", true);
    // Tokens of their own, so revoking one leaves the fixture's token (and its calls) working.
    let bots_token = forgejo.add_token("bots");
    let first_token = forgejo.add_token("work-1");

    let rt = tokio::runtime::Handle::current();
    let proxy =
        tokio::task::block_in_place(|| rt.block_on(NameProxy::start(&[(NAME, forgejo.addr())])));
    let options = with_fake_names(
        through_company_proxy(support::named_id_options(), &proxy),
        [NAME],
    );
    let platform = platform_trusting([forgejo.root()]);
    let vm = start_vm_host_platform("fgc", options, &platform, |_| {}).await;
    let name = format!("{}-fgc", vm.settings.prefix.as_str());
    let sandbox = WorkspaceId::new(&name).unwrap();
    puddle_store::testing::own(vm.host.store(), &sandbox);
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
    let api = Api::new(vm.host.url(), vm.host.token());
    let mut events = api.events().await;

    // The only identity covers another owner on the forge. Its token would open acme's private
    // repositories too, so a clone that works means Puddle sent a credential it must not.
    let bots_source = keep_token(&api, &bots_token).await;
    let made = api
        .post("/api/identities", &identity("Bots", &bots_source, "bots"))
        .await;
    assert_eq!(made.status, 201, "{}", made.body);

    // Made from a private repository no identity covers, the workspace starts without it and
    // a problem holds git's own reason.
    let app_url = format!("https://{NAME}/{FORGEJO_OWNER}/app.git");
    let name_static: &'static str = Box::leak(name.clone().into_boxed_str());
    let reply = api
        .post(
            "/api/workspaces",
            &json!({"name": name, "repositories": [{"url": app_url}], "memory_mib": 2048})
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
    let problems = api.get("/api/problems").await.json();
    let clone_problem = problems["problems"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| {
            p["workspace"] == name.as_str() && p["title"].as_str().unwrap().contains(&app_url)
        })
        .unwrap_or_else(|| panic!("no clone problem for {app_url}: {problems}"))
        .clone();
    let detail = clone_problem["detail"].as_str().unwrap();
    assert!(
        detail.contains("git exited 128")
            && detail.contains(&format!(
                "fatal: could not read Username for 'https://{NAME}'"
            )),
        "the problem holds git's reason: {detail}"
    );
    let anonymous = forge_connections(&api, &name, 0, |rows| !rows.is_empty()).await;
    assert!(
        !anonymous.is_empty(),
        "the clone reached the forge through the proxy"
    );
    assert!(
        anonymous
            .iter()
            .all(|r| r["injected"] == false && r["binding_id"].is_null()),
        "no credential went with a request no identity covers: {anonymous:#?}"
    );
    let booted = boot_id(&vm, &sandbox).await;

    // The guest trusts the forge's root as it is, no `-k`.
    let code = guest(
        &vm.runtime,
        &sandbox,
        &format!("curl -sS -o /dev/null -w '%{{http_code}}' https://{NAME}/"),
    )
    .await;
    assert_eq!(code.trim(), "200", "{code}");

    // An identity for acme, added to the running workspace, clones at once.
    let first_source = keep_token(&api, &first_token).await;
    let made = api
        .post(
            "/api/identities",
            &identity("Work", &first_source, FORGEJO_OWNER),
        )
        .await;
    assert_eq!(made.status, 201, "{}", made.body);
    let work_id = made.json()["id"].as_i64().unwrap();
    let attached = api
        .post(
            &format!("/api/workspaces/{name}/identities"),
            &json!({"identity": work_id}).to_string(),
        )
        .await;
    assert_eq!(attached.status, 200, "{}", attached.body);
    guest(
        &vm.runtime,
        &sandbox,
        &format!(
            "set -eu; rm -rf /tmp/f; mkdir /tmp/f; cd /tmp/f; git clone -q {app_url} app; cd app; \
             git config user.name Work; git config user.email work@example.org"
        ),
    )
    .await;

    // A day of pushes. Each lands, and each is one connection record with the credential
    // injected (the record names the connection's first request: the push's ref listing).
    let mark = audit_mark(&api, &name).await;
    let out = guest(
        &vm.runtime,
        &sandbox,
        "set -eu; cd /tmp/f/app; for i in 1 2 3 4 5; do echo $i > day.txt; git add day.txt; \
         git commit -q -m \"day $i\"; git push -q origin HEAD:refs/heads/team-day; done; \
         echo head=$(git rev-parse HEAD)",
    )
    .await;
    let pushed = head_of(&out).to_owned();
    assert_eq!(
        forgejo.branch_head("acme/app", "team-day").as_deref(),
        Some(pushed.as_str()),
        "the last push reached the forge\n{}",
        forgejo.log()
    );
    let day_rows = forge_connections(&api, &name, mark, |rows| rows.len() >= 5).await;
    assert_eq!(day_rows.len(), 5, "one record per push: {day_rows:#?}");
    for row in &day_rows {
        assert_eq!(row["injected"], true, "{row:#}");
        assert!(
            row["path"]
                .as_str()
                .is_some_and(|p| p.starts_with("/acme/app.git/")),
            "{row:#}"
        );
    }

    // A push that is not a fast-forward is refused by git as it would be without Puddle.
    let out = guest(
        &vm.runtime,
        &sandbox,
        "cd /tmp/f/app; git reset -q --hard HEAD~1; echo other > day.txt; \
         git commit -q -am diverged; git push origin HEAD:refs/heads/team-day 2>&1; echo exit=$?",
    )
    .await;
    assert!(
        out.contains("[rejected]")
            && (out.contains("(non-fast-forward)") || out.contains("(fetch first)"))
            && !out.contains("exit=0"),
        "{out}"
    );
    assert!(!out.contains("puddle"), "Puddle added nothing: {out}");
    assert_eq!(
        forgejo.branch_head("acme/app", "team-day").as_deref(),
        Some(pushed.as_str()),
        "the forge kept its branch"
    );

    // A push to a repository off the push list is refused and the forge is
    // unchanged; once push is turned on for it, the same guest's next push lands.
    let other_head = forgejo.branch_head("acme/other", "main").unwrap();
    let refused = guest(
        &vm.runtime,
        &sandbox,
        &format!(
            "cd /tmp/f; git clone -q https://{NAME}/acme/other.git other; cd other; \
             git config user.name Work; git config user.email work@example.org; \
             echo x > x.txt; git add x.txt; git commit -q -m x; \
             git push origin HEAD:main 2>&1; echo exit=$?"
        ),
    )
    .await;
    assert!(
        refused.contains("not in this workspace's allowed repositories")
            && !refused.contains("exit=0"),
        "{refused}"
    );
    assert_eq!(
        forgejo.branch_head("acme/other", "main").unwrap(),
        other_head,
        "the refused push changed nothing on the forge"
    );
    let listed = api
        .post(
            &format!("/api/workspaces/{name}/git/repos"),
            &json!({"host": NAME, "owner": FORGEJO_OWNER, "repo": "other", "pull": true, "push": true})
                .to_string(),
        )
        .await;
    assert_eq!(listed.status, 201, "{}", listed.body);
    let out = guest(
        &vm.runtime,
        &sandbox,
        "set -eu; cd /tmp/f/other; git push -q origin HEAD:main; echo head=$(git rev-parse HEAD)",
    )
    .await;
    assert_eq!(
        forgejo.branch_head("acme/other", "main").as_deref(),
        Some(head_of(&out)),
        "the push landed once push was on\n{}",
        forgejo.log()
    );

    // The token is revoked on the forge. The next fetch is a 403 that names the
    // identity, and one sign-in notice is raised however often git asks.
    forgejo.revoke_token("work-1");
    let out = guest(
        &vm.runtime,
        &sandbox,
        "cd /tmp/f/app; git fetch 2>&1; echo exit=$?; git fetch 2>&1; echo exit=$?",
    )
    .await;
    assert!(
        out.contains(&format!(
            "{NAME} did not accept the credential of identity Work"
        )) && out.contains("403")
            && !out.contains("exit=0"),
        "{out}"
    );
    let notices = count_within(&mut events, Duration::from_secs(5), |e| {
        e["type"] == "credential_sign_in_needed" && e["host"] == NAME
    })
    .await;
    assert_eq!(notices, 1, "one sign-in notice: {:#?}", events.seen);

    // A new token pasted into the identity: the next fetch and push use it, the old one leaves
    // the credential store.
    let second_token = forgejo.add_token("work-2");
    let second_source = keep_token(&api, &second_token).await;
    let replaced = api
        .put(
            &format!("/api/identities/{work_id}"),
            &identity("Work", &second_source, FORGEJO_OWNER),
        )
        .await;
    assert_eq!(replaced.status, 200, "{}", replaced.body);
    let first_id = StoredId::new(first_source["id"].as_str().unwrap()).unwrap();
    assert!(
        vm.secrets.get(&first_id).unwrap().is_none(),
        "the replaced token left the credential store"
    );
    let out = guest(
        &vm.runtime,
        &sandbox,
        "set -eu; cd /tmp/f/app; git fetch -q; git checkout -q -b after-new-token origin/team-day; \
         echo again > day.txt; git commit -q -am again; \
         git push -q origin HEAD:refs/heads/after-new-token; echo head=$(git rev-parse HEAD)",
    )
    .await;
    assert_eq!(
        forgejo
            .branch_head("acme/app", "after-new-token")
            .as_deref(),
        Some(head_of(&out)),
        "the push with the new token landed\n{}",
        forgejo.log()
    );

    assert_eq!(
        boot_id(&vm, &sandbox).await,
        booted,
        "the workspace never stopped"
    );
    assert_guest_holds_no_token(&vm, &sandbox, &[&first_token, &second_token, &bots_token]).await;

    let _ = proxy.connects();
    let down = vm.host.shutdown().await;
    assert!(down.sandboxes.all_stopped(), "{down:?}");
}
