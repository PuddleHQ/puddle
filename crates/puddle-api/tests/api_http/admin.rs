// SPDX-License-Identifier: AGPL-3.0-or-later
//! The owner's admin screen over real HTTP: making a user with a one-time invite, the list with its
//! counts, reissuing and deleting, the audit, and that nobody else reaches any of it and nothing it
//! answers can carry a credential.

use std::sync::Arc;
use std::time::Duration;

use puddle_services::{fake_workspaces::FakeWorkspaces, workspaces::WorkspaceService};
use serde_json::{Value, json};

use super::sign_in::{Req, Site, access_token, pw, site_reaping};

/// The reaper's timer in these tests, so a look that no notice caused comes soon.
const REAP_EVERY: Duration = Duration::from_millis(10);

const REPO: &str = "https://github.com/acme/api.git";

struct Admin {
    site: Site,
    workspaces: Arc<FakeWorkspaces>,
}

async fn admin_site(multi_user: bool) -> Admin {
    admin_site_slow(multi_user, Duration::ZERO).await
}

/// Like [`admin_site`], with a pause between the steps of a workspace operation so that an
/// operation is still under way when the next look comes.
async fn admin_site_slow(multi_user: bool, step_delay: Duration) -> Admin {
    let made: std::sync::Mutex<Option<Arc<FakeWorkspaces>>> = std::sync::Mutex::new(None);
    let site = site_reaping(multi_user, REAP_EVERY, |events, clock| {
        let fake = Arc::new(FakeWorkspaces::with_options(
            events.clone(),
            clock.clone(),
            Arc::new(puddle_services::fake_workspaces::FakeLauncher::new()),
            step_delay,
        ));
        *made.lock().unwrap() = Some(fake.clone());
        Some(fake)
    })
    .await;
    let workspaces = made.lock().unwrap().take().unwrap();
    Admin { site, workspaces }
}

impl Admin {
    /// The owner as the desktop window or the command line on the server: the install token on
    /// the loopback address.
    async fn owner(&self, method: &str, path: &str, body: Option<&Value>) -> super::common::Reply {
        let host = self.site.loopback_host();
        self.site
            .send(
                method,
                path,
                &Req::bearer(&self.site.install_token).on(&host),
                body,
            )
            .await
    }

    /// Makes `name` through the admin API and has them choose `password` with the invite.
    /// Returns their access token and the user id.
    async fn user(&self, name: &str, password: &str) -> (String, i64) {
        let made = self
            .owner("POST", "/api/admin/users", Some(&json!({"name": name})))
            .await;
        assert_eq!(made.status, 201, "{made:?}");
        let secret = made.json()["invite_secret"].as_str().unwrap().to_owned();
        let id = made.json()["user"]["id"].as_i64().unwrap();
        let joined = self
            .site
            .send(
                "POST",
                "/api/auth/invite",
                &Req::page(),
                Some(&json!({"secret": secret, "password": password, "device_name": "browser"})),
            )
            .await;
        assert_eq!(joined.status, 200, "{joined:?}");
        (access_token(&joined), id)
    }

    async fn listed(&self) -> Vec<Value> {
        let list = self.owner("GET", "/api/admin/users", None).await;
        assert_eq!(list.status, 200, "{list:?}");
        list.json()["users"].as_array().unwrap().clone()
    }

    /// Waits until the background deletion has removed the user.
    async fn gone(&self, name: &str) {
        // The reaper also looks on a timer (REAP_EVERY here), so a missed notice costs up to that.
        puddle_test_kit::eventually_async(
            &format!("{name} is removed"),
            Duration::from_secs(30),
            || async { !self.listed().await.iter().any(|u| u["name"] == name) },
        )
        .await;
    }
}

const ROUTES: &[(&str, &str)] = &[
    ("GET", "/api/admin/users"),
    ("GET", "/api/admin/stats"),
    ("GET", "/api/admin/audit"),
    ("POST", "/api/admin/users"),
    ("POST", "/api/admin/users/2/invite"),
    ("DELETE", "/api/admin/users/2?confirm=ann"),
];

#[tokio::test]
async fn a_bad_path_or_query_is_answered_with_the_json_error_body() {
    let admin = admin_site(true).await;
    admin.site.setup().await;
    for (method, path) in [
        ("POST", "/api/admin/users/x/invite"),
        ("DELETE", "/api/admin/users/x?confirm=ann"),
        ("DELETE", "/api/admin/users/2"),
    ] {
        let reply = admin.owner(method, path, None).await;
        assert_eq!(reply.status, 400, "{method} {path}: {reply:?}");
        let body = reply.json();
        assert_eq!(body["error"], "bad_request", "{method} {path}: {reply:?}");
        assert!(body["message"].is_string(), "{method} {path}: {reply:?}");
    }
}

#[tokio::test]
async fn a_new_user_is_invited_never_given_a_password_and_chooses_their_own() {
    let admin = admin_site(true).await;
    admin.site.setup().await;
    let made = admin
        .owner("POST", "/api/admin/users", Some(&json!({"name": "Ann"})))
        .await;
    assert_eq!(made.status, 201, "{made:?}");
    assert_eq!(made.header("cache-control"), Some("no-store"));
    let body = made.json();
    assert_eq!(body["invite_secret"].as_str().unwrap().len(), 43);
    assert_eq!(body["user"]["name"], "Ann");
    assert_eq!(body["user"]["state"], "invited");
    assert!(body["expires_at"].as_u64().unwrap() > 0);

    // Invited: no password, so the name signs in nowhere.
    let list = admin.listed().await;
    assert_eq!(list.len(), 2);
    assert_eq!(list[1]["state"], "invited");
    assert_eq!(list[1]["devices"], 0);
    assert_eq!(list[1]["last_sign_in"], Value::Null);
    let attempt = admin.site.sign_in("Ann", pw("anything at all goes")).await;
    assert_eq!(attempt.status, 401, "{attempt:?}");

    // The invite works once, for the person who opens it.
    let secret = body["invite_secret"].as_str().unwrap();
    let chosen = pw("ann's own choice");
    let join = json!({"secret": secret, "password": chosen});
    let first = admin
        .site
        .send("POST", "/api/auth/invite", &Req::page(), Some(&join))
        .await;
    assert_eq!(first.status, 200, "{first:?}");
    let second = admin
        .site
        .send("POST", "/api/auth/invite", &Req::page(), Some(&join))
        .await;
    assert_eq!(second.status, 401, "{second:?}");
    assert_eq!(admin.site.sign_in("Ann", chosen).await.status, 200);

    let list = admin.listed().await;
    assert_eq!(list[1]["state"], "active");
    assert_eq!(list[1]["devices"], 2);
    assert!(list[1]["last_sign_in"].as_u64().is_some());
    let taken = admin
        .owner("POST", "/api/admin/users", Some(&json!({"name": "ANN"})))
        .await;
    assert_eq!(taken.status, 422, "{taken:?}");
    admin.site.running.shutdown().await;
}

#[tokio::test]
async fn nobody_but_the_owner_reaches_an_admin_route() {
    let admin = admin_site(true).await;
    admin.site.setup().await;
    let (ann, _) = admin.user("Ann", pw("ann password one")).await;
    for (method, path) in ROUTES {
        let body = (*method == "POST").then(|| json!({"name": "Eve"}));
        let reply = admin
            .site
            .send(method, path, &Req::bearer(&ann), body.as_ref())
            .await;
        assert_eq!(reply.status, 403, "{method} {path}: {reply:?}");
        let anonymous = admin
            .site
            .send(method, path, &Req::default(), body.as_ref())
            .await;
        assert_eq!(anonymous.status, 401, "{method} {path}: {anonymous:?}");
    }
    assert_eq!(admin.listed().await.len(), 2, "Ann made nobody");
    admin.site.running.shutdown().await;
}

#[tokio::test]
async fn with_several_users_off_the_admin_routes_do_not_exist() {
    let admin = admin_site(false).await;
    for (method, path) in ROUTES {
        let body = (*method == "POST").then(|| json!({"name": "Eve"}));
        let reply = admin.owner(method, path, body.as_ref()).await;
        assert_eq!(reply.status, 404, "{method} {path}: {reply:?}");
    }
    admin.site.running.shutdown().await;
}

#[tokio::test]
async fn the_list_counts_workspaces_and_disk_and_never_names_them() {
    let admin = admin_site(true).await;
    admin.site.setup().await;
    let (ann, _) = admin.user("Ann", pw("ann password two")).await;
    for name in ["secret project", "other"] {
        let made = admin
            .site
            .send(
                "POST",
                "/api/workspaces",
                &Req::bearer(&ann),
                Some(&json!({"name": name, "repositories": [{"url": REPO}]})),
            )
            .await;
        assert_eq!(made.status, 202, "{made:?}");
    }
    admin.workspaces.idle().await;
    let list = admin.listed().await;
    assert_eq!(list[0]["workspaces_running"].as_u64().unwrap(), 0);
    // A create ends running.
    assert_eq!(
        list[1]["workspaces_running"].as_u64().unwrap(),
        2,
        "{list:?}"
    );
    assert_eq!(list[1]["workspaces_stopped"].as_u64().unwrap(), 0);
    let stats = admin.owner("GET", "/api/admin/stats", None).await;
    assert_eq!(stats.status, 200, "{stats:?}");
    assert_eq!(stats.json()["users"], 2);
    assert_eq!(stats.json()["workspaces"], 2);
    for reply in [
        admin.owner("GET", "/api/admin/users", None).await,
        stats,
        admin.owner("GET", "/api/admin/audit", None).await,
    ] {
        assert!(
            !reply.body.contains("secret project") && !reply.body.contains(REPO),
            "{reply:?}"
        );
    }
    admin.site.running.shutdown().await;
}

#[tokio::test]
async fn deleting_a_user_needs_their_name_ends_their_sessions_and_removes_their_workspaces() {
    let admin = admin_site(true).await;
    admin.site.setup().await;
    let (ann, id) = admin.user("Ann", pw("ann password three")).await;
    let (bob, bob_id) = admin.user("Bob", pw("bob password three")).await;
    for (token, name) in [(&ann, "ann-box"), (&bob, "bob-box")] {
        let made = admin
            .site
            .send(
                "POST",
                "/api/workspaces",
                &Req::bearer(token),
                Some(&json!({"name": name, "repositories": [{"url": REPO}]})),
            )
            .await;
        assert_eq!(made.status, 202, "{made:?}");
    }
    admin.workspaces.idle().await;
    assert_eq!(admin.workspaces.list().await.unwrap().len(), 2);

    let wrong = admin
        .owner(
            "DELETE",
            &format!("/api/admin/users/{id}?confirm=ann"),
            None,
        )
        .await;
    assert_eq!(wrong.status, 422, "the name is typed exactly: {wrong:?}");
    let me = admin
        .owner("DELETE", "/api/admin/users/1?confirm=Tijs", None)
        .await;
    assert_eq!(me.status, 409, "no deleting yourself: {me:?}");
    let nobody = admin
        .owner("DELETE", "/api/admin/users/99?confirm=x", None)
        .await;
    assert_eq!(nobody.status, 404, "{nobody:?}");

    let done = admin
        .owner(
            "DELETE",
            &format!("/api/admin/users/{id}?confirm=Ann"),
            None,
        )
        .await;
    assert_eq!(done.status, 202, "{done:?}");
    // Locked out at once: the token and the password are dead.
    let after = admin
        .site
        .send("GET", "/api/pending", &Req::bearer(&ann), None)
        .await;
    assert_eq!(after.status, 401, "{after:?}");
    assert_eq!(
        admin
            .site
            .sign_in("Ann", pw("ann password three"))
            .await
            .status,
        401
    );
    admin.gone("Ann").await;
    admin.workspaces.idle().await;

    // Only Ann's workspace went; Bob still has his and can still work.
    let left = admin.workspaces.list().await.unwrap();
    assert_eq!(left.len(), 1, "{left:?}");
    assert_eq!(
        admin
            .site
            .store
            .workspaces_of(puddle_store::UserId::from_stored(id))
            .unwrap(),
        Vec::new()
    );
    let bobs = admin
        .site
        .send("GET", "/api/workspaces", &Req::bearer(&bob), None)
        .await;
    assert_eq!(bobs.json()["workspaces"].as_array().unwrap().len(), 1);
    assert_eq!(admin.listed().await.len(), 2);

    // The audit says who did what to whom, with the names as they were, and nothing else.
    let audit = admin.owner("GET", "/api/admin/audit", None).await;
    let lines = audit.json()["lines"].as_array().unwrap().clone();
    let story: Vec<(&str, &str)> = lines
        .iter()
        .map(|l| (l["action"].as_str().unwrap(), l["target"].as_str().unwrap()))
        .collect();
    assert_eq!(
        story,
        [
            ("user_deleted", "Ann"),
            ("user_delete_started", "Ann"),
            ("user_created", "Bob"),
            ("user_created", "Ann"),
        ]
    );
    assert_eq!(lines[0]["actor"], "Tijs");
    assert_eq!(bob_id, 3, "ids are never reused");
    // The owner is never deletable, and a gone user is just unknown.
    let owner = admin
        .owner("DELETE", "/api/admin/users/1?confirm=Tijs", None)
        .await;
    assert_eq!(owner.status, 409);
    admin.site.running.shutdown().await;
}

#[tokio::test]
async fn reissuing_ends_the_password_and_the_sessions_and_replaces_the_invite() {
    let admin = admin_site(true).await;
    admin.site.setup().await;
    let before = pw("ann before the reissue");
    let (ann, id) = admin.user("Ann", before).await;

    let again = admin
        .owner("POST", &format!("/api/admin/users/{id}/invite"), None)
        .await;
    assert_eq!(again.status, 200, "{again:?}");
    assert_eq!(again.header("cache-control"), Some("no-store"));
    assert_eq!(again.json()["user"]["state"], "invited");
    // The old password and the old browser are dead.
    assert_eq!(admin.site.sign_in("Ann", before).await.status, 401);
    let dead = admin
        .site
        .send("GET", "/api/pending", &Req::bearer(&ann), None)
        .await;
    assert_eq!(dead.status, 401, "{dead:?}");
    assert_eq!(admin.listed().await[1]["state"], "invited");

    // A second reissue replaces the first invite.
    let first = again.json()["invite_secret"].as_str().unwrap().to_owned();
    let newer = admin
        .owner("POST", &format!("/api/admin/users/{id}/invite"), None)
        .await;
    let join = |secret: &str| json!({"secret": secret, "password": pw("ann after the reissue")});
    let stale = admin
        .site
        .send(
            "POST",
            "/api/auth/invite",
            &Req::page(),
            Some(&join(&first)),
        )
        .await;
    assert_eq!(stale.status, 401, "{stale:?}");
    let fresh = newer.json()["invite_secret"].as_str().unwrap().to_owned();
    let ok = admin
        .site
        .send(
            "POST",
            "/api/auth/invite",
            &Req::page(),
            Some(&join(&fresh)),
        )
        .await;
    assert_eq!(ok.status, 200, "{ok:?}");

    let missing = admin
        .owner("POST", "/api/admin/users/99/invite", None)
        .await;
    assert_eq!(missing.status, 404);
    let owner = admin.owner("POST", "/api/admin/users/1/invite", None).await;
    assert_eq!(
        owner.status, 409,
        "the owner is reset on the server: {owner:?}"
    );
    let lines = admin.owner("GET", "/api/admin/audit", None).await.json();
    let reissued = lines["lines"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|l| l["action"] == "invite_reissued")
        .count();
    assert_eq!(reissued, 2);
    admin.site.running.shutdown().await;
}

/// Nothing an admin route answers, and nothing the audit holds, carries a password, a hash, a
/// session or an invite secret after the answer that made the invite.
#[tokio::test]
async fn no_admin_answer_but_the_new_invite_carries_a_secret() {
    let admin = admin_site(true).await;
    let owner = admin.site.setup().await;
    let (ann, id) = admin.user("Ann", pw("ann password four")).await;
    let reissued = admin
        .owner("POST", &format!("/api/admin/users/{id}/invite"), None)
        .await;
    let secret = reissued.json()["invite_secret"]
        .as_str()
        .unwrap()
        .to_owned();
    let cookie = super::sign_in::set_cookie(&owner).0;
    let forbidden = [
        secret.as_str(),
        pw("ann password four"),
        pw("correct horse battery"),
        ann.as_str(),
        cookie.as_str(),
        "argon2",
        "refresh",
    ];
    for path in ["/api/admin/users", "/api/admin/stats", "/api/admin/audit"] {
        let reply = admin.owner("GET", path, None).await;
        for word in forbidden {
            assert!(
                !reply.body.contains(word),
                "{path} carries {word}: {reply:?}"
            );
        }
        for key in [
            "invite_secret",
            "digest",
            "hash",
            "password",
            "token",
            "session",
        ] {
            assert!(
                !reply.body.to_lowercase().contains(key),
                "{path} names {key}"
            );
        }
    }
    admin.site.running.shutdown().await;
}

async fn make_box(admin: &Admin, token: &str, name: &str) {
    let made = admin
        .site
        .send(
            "POST",
            "/api/workspaces",
            &Req::bearer(token),
            Some(&json!({"name": name, "repositories": [{"url": REPO}]})),
        )
        .await;
    assert_eq!(made.status, 202, "{made:?}");
    admin.workspaces.idle().await;
}

async fn start_delete(admin: &Admin, id: i64, name: &str) {
    let done = admin
        .owner(
            "DELETE",
            &format!("/api/admin/users/{id}?confirm={name}"),
            None,
        )
        .await;
    assert_eq!(done.status, 202, "{done:?}");
}

#[tokio::test]
async fn a_deletion_whose_workspace_step_fails_is_finished_at_a_later_look() {
    let admin = admin_site(true).await;
    admin.site.setup().await;
    let (ann, ann_id) = admin.user("Ann", pw("ann password three")).await;
    make_box(&admin, &ann, "ann-box").await;
    // A create ends running, so her box is up already.
    let mut others = Vec::new();
    for name in ["Bob", "Cy", "Dee", "Eve"] {
        others.push((name, admin.user(name, pw("other password three")).await.1));
    }

    // The stop fails and later the delete fails: Ann stays as long as her box is left.
    admin.workspaces.fail_next(
        puddle_services::workspaces::Operation::Stopping,
        "the sandbox hangs",
    );
    start_delete(&admin, ann_id, "Ann").await;
    admin.workspaces.idle().await;
    admin.workspaces.fail_next(
        puddle_services::workspaces::Operation::Deleting,
        "the volume is locked",
    );
    // Ann needs four looks: the stop fails, the stop works, the delete fails, the delete works.
    // A deletion started while a look runs only queues one more look, so two deletions in a row
    // can share a look. Waiting until each other user is gone gives one look per deletion, and
    // the one that follows Ann's own deletion is the first of the four.
    for (name, id) in others {
        start_delete(&admin, id, name).await;
        admin.gone(name).await;
        admin.workspaces.idle().await;
    }
    admin.gone("Ann").await;
    assert_eq!(admin.workspaces.list().await.unwrap(), Vec::new());
    admin.site.running.shutdown().await;
}

#[tokio::test]
async fn a_workspace_still_busy_is_left_alone_until_it_is_idle() {
    let admin = admin_site_slow(true, Duration::from_millis(30)).await;
    admin.site.setup().await;
    let (ann, ann_id) = admin.user("Ann", pw("ann password three")).await;
    let (_bob, bob_id) = admin.user("Bob", pw("bob password three")).await;
    make_box(&admin, &ann, "ann-box").await;

    start_delete(&admin, ann_id, "Ann").await;
    // A user who is going cannot be given a new invite.
    let again = admin
        .owner("POST", &format!("/api/admin/users/{ann_id}/invite"), None)
        .await;
    assert_eq!(again.status, 409, "{again:?}");
    // The stop is under way (the box is busy); this second look must leave it alone.
    start_delete(&admin, bob_id, "Bob").await;
    admin.gone("Ann").await;
    admin.gone("Bob").await;
    admin.workspaces.idle().await;
    assert_eq!(admin.workspaces.list().await.unwrap(), Vec::new());
    admin.site.running.shutdown().await;
}

#[tokio::test]
async fn a_workspace_that_refuses_its_stop_is_tried_again_at_a_later_look() {
    let admin = admin_site(true).await;
    admin.site.setup().await;
    let (ann, ann_id) = admin.user("Ann", pw("ann password three")).await;
    let (_bob, bob_id) = admin.user("Bob", pw("bob password three")).await;
    make_box(&admin, &ann, "ann-box").await;
    // A paused box counts as up but cannot be stopped: the stop is refused at once.
    let mut record = admin.workspaces.list().await.unwrap().remove(0);
    record.status = puddle_types::WorkspaceStatus::Paused;
    admin.workspaces.seed(
        record.clone(),
        puddle_services::fake_workspaces::Unsaved::default(),
    );

    start_delete(&admin, ann_id, "Ann").await;
    // An absence window: nothing signals that the reaper looked and left Ann, so give it time.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(admin.listed().await.iter().any(|u| u["name"] == "Ann"));

    record.status = puddle_types::WorkspaceStatus::Stopped;
    admin
        .workspaces
        .seed(record, puddle_services::fake_workspaces::Unsaved::default());
    start_delete(&admin, bob_id, "Bob").await;
    admin.gone("Ann").await;
    admin.gone("Bob").await;
    admin.workspaces.idle().await;
    assert_eq!(admin.workspaces.list().await.unwrap(), Vec::new());
    admin.site.running.shutdown().await;
}

/// One person of the three: the workspace, the rule's host and the id the API gave the workspace.
struct Person<'a> {
    name: &'a str,
    workspace: &'a str,
    host: &'a str,
    token: &'a str,
    id: String,
}

/// Three people on one install, each with a workspace, a rule and the records they make: nobody's
/// Activity, workspace list or rules hold a word of another's, and one's id for another's
/// workspace is answered exactly as an id nobody has, for the workspace, its audit and its rules.
#[expect(
    clippy::too_many_lines,
    reason = "one story: three people make things, then each reads and probes the others'"
)]
#[tokio::test]
async fn three_users_see_nothing_of_each_other_and_a_foreign_id_is_an_unknown_one() {
    let admin = admin_site(true).await;
    admin.site.setup().await;
    let (lee, _) = admin.user("Lee", pw("lee password three")).await;
    let (ayo, _) = admin.user("Ayo", pw("ayo password three")).await;
    let owner = admin.site.install_token.clone();
    let loopback = admin.site.loopback_host();

    // Each person's request goes in their own way: the owner as the desktop does, on loopback.
    let mut people = [
        Person {
            name: "owner",
            workspace: "morgan-tools",
            host: "morgan-only.example",
            token: &owner,
            id: String::new(),
        },
        Person {
            name: "lee",
            workspace: "lee-api",
            host: "lee-only.example",
            token: &lee,
            id: String::new(),
        },
        Person {
            name: "ayo",
            workspace: "ayo-web",
            host: "ayo-only.example",
            token: &ayo,
            id: String::new(),
        },
    ];
    let send = |person_token: String, method: &'static str, path: String, body: Option<Value>| {
        let (admin, loopback, owner) = (&admin, loopback.clone(), owner.clone());
        async move {
            let req = if person_token == owner {
                Req::bearer(&person_token).on(&loopback)
            } else {
                Req::bearer(&person_token)
            };
            let reply = admin.site.send(method, &path, &req, body.as_ref()).await;
            (reply.status, reply.body)
        }
    };
    for person in &mut people {
        let (status, body) = send(
            person.token.to_owned(),
            "POST",
            "/api/workspaces".to_owned(),
            Some(json!({"name": person.workspace, "repositories": [{"url": REPO}]})),
        )
        .await;
        assert_eq!(status, 202, "{}: {body}", person.name);
        person.id = serde_json::from_str::<Value>(&body).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let (status, body) = send(
            person.token.to_owned(),
            "POST",
            "/api/rules".to_owned(),
            Some(json!({"scope": {"type": "global"}, "pattern": person.host, "effect": "allow"})),
        )
        .await;
        assert_eq!(status, 201, "{}: {body}", person.name);
    }
    admin.workspaces.idle().await;

    // S6a: what each reads holds their own (the control) and nothing of the other two.
    for me in &people {
        for path in ["/api/workspaces", "/api/audit?limit=500", "/api/rules"] {
            let (status, body) = send(me.token.to_owned(), "GET", path.to_owned(), None).await;
            assert_eq!(status, 200, "{} {path}: {body}", me.name);
            let own = if path == "/api/workspaces" {
                me.workspace
            } else {
                me.host
            };
            assert!(
                body.contains(own),
                "{} {path} lacks their own {own}: {body}",
                me.name
            );
            for other in people.iter().filter(|o| o.name != me.name) {
                for word in [other.workspace, other.host, other.id.as_str()] {
                    assert!(
                        !body.contains(word),
                        "{} {path} holds {word} of {}: {body}",
                        me.name,
                        other.name
                    );
                }
            }
        }
    }

    // S6b: another's workspace id answers as an id nobody has.
    let unknown = "nobody00";
    for me in &people {
        for other in people.iter().filter(|o| o.name != me.name) {
            let rule_for = |workspace: &str| json!({"scope": {"type": "workspace", "workspace": workspace}, "pattern": "x.example", "effect": "allow"});
            let probes: [fn(&str) -> String; 4] = [
                |id| format!("/api/workspaces/{id}"),
                |id| format!("/api/workspaces/{id}/delete-check"),
                |id| format!("/api/audit?workspace={id}"),
                |id| format!("/api/workspaces/{id}/interception"),
            ];
            for path in probes {
                let method = "GET";
                let theirs = send(me.token.to_owned(), method, path(&other.id), None).await;
                let nobodys = send(me.token.to_owned(), method, path(unknown), None).await;
                let same =
                    |(status, body): (u16, String), id: &str| (status, body.replace(id, "ID"));
                assert_eq!(
                    same(theirs.clone(), &other.id),
                    same(nobodys, unknown),
                    "{} asking {method} {} for {}'s id",
                    me.name,
                    path("ID"),
                    other.name
                );
                if !path("ID").starts_with("/api/audit") {
                    assert_eq!(theirs.0, 404, "{}: {}", path("ID"), theirs.1);
                }
            }
            let theirs = send(
                me.token.to_owned(),
                "POST",
                "/api/rules".to_owned(),
                Some(rule_for(&other.id)),
            )
            .await;
            let nobodys = send(
                me.token.to_owned(),
                "POST",
                "/api/rules".to_owned(),
                Some(rule_for(unknown)),
            )
            .await;
            assert_eq!(
                theirs.0, 404,
                "a rule for {}'s workspace: {}",
                other.name, theirs.1
            );
            assert_eq!(
                theirs.1.replace(&other.id, "ID"),
                nobodys.1.replace(unknown, "ID")
            );
        }
    }
    admin.site.running.shutdown().await;
}
