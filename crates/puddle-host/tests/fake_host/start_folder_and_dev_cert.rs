// SPDX-License-Identifier: AGPL-3.0-or-later
//! The start folder, the multi-root file and the development certificate.

use super::*;

#[tokio::test(flavor = "multi_thread")]
async fn a_workspace_boots_with_the_development_certificate_and_a_renewed_one_at_its_next_start() {
    let rig = Rig::named_ids();
    rig.dev_cert.give();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;

    let first = rig.guest.boots() - 1;
    let plan = String::from_utf8(rig.guest.plan_bytes(first)).unwrap();
    let files = rig.guest.plan_files(first);
    // The certificate is public; the key is a file only root reads, and a step sets up .NET.
    assert!(plan.contains("puddle_file '/etc/puddle/dev-cert/localhost.key' 0600 "));
    assert!(plan.contains("puddle_file '/etc/puddle/dev-cert/localhost.crt' 0644 "));
    assert!(plan.contains("puddle_step '/usr/local/lib/puddle/dev-cert.sh'"));
    let cert = pem_of(&files, "/etc/puddle/dev-cert/localhost.crt");
    assert!(pem_of(&files, "/etc/puddle/dev-cert/localhost.key").contains("BEGIN PRIVATE KEY"));
    assert_eq!(
        pem_of(
            &files,
            "/usr/local/share/ca-certificates/puddle-dev/localhost.crt"
        ),
        cert
    );
    assert!(pem_of(&files, "/etc/puddle/extra-cas.pem").contains(&cert));
    let env = pem_of(&files, "/etc/profile.d/01-puddle-env.sh");
    for line in [
        "export DEV_CERT_PEM='/etc/puddle/dev-cert/localhost.crt'",
        "export DEV_KEY_PEM='/etc/puddle/dev-cert/localhost.key'",
        "export DOTNET_GENERATE_ASPNET_CERTIFICATE='false'",
    ] {
        assert!(env.contains(line), "{line} in {env}");
    }
    assert!(env.contains("export SSL_CERT_DIR='/home/agent/.aspnet/dev-certs/trust:"));

    // After a renewal the next start (the guest boots again) carries the new certificate.
    rig.dev_cert.renew();
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let renewed = pem_of(
        &rig.guest.plan_files(rig.guest.boots() - 1),
        "/etc/puddle/dev-cert/localhost.crt",
    );
    assert_ne!(renewed, cert);
}

/// A declined system confirmation is asked again at a start (the spec's choice), never at an
/// attach: closing an editor window and opening one again must not raise it a second time.
#[tokio::test(flavor = "multi_thread")]
async fn only_a_start_asks_for_the_development_certificate_never_an_attach() {
    let rig = Rig::named_ids();
    rig.dev_cert.give();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    assert_eq!(rig.dev_cert.asked(), 1, "the start asked once");
    allow_direct_ssh(&api, "acme", true).await;
    for mode in ["browser", "desktop", "browser"] {
        let reply = api
            .post(
                "/api/workspaces/acme/attach",
                &json!({"mode": mode}).to_string(),
            )
            .await;
        assert_eq!(reply.json()["opened"], true, "{mode}: {}", reply.body);
    }
    assert_eq!(rig.dev_cert.asked(), 1, "three attaches asked nothing more");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_host_without_a_development_certificate_writes_nothing_for_it_into_the_guest() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;

    let plan = String::from_utf8(rig.guest.plan_bytes(rig.guest.boots() - 1)).unwrap();
    for word in ["dev-cert", "puddle-dev", "DEV_CERT_PEM", "DOTNET_GENERATE"] {
        assert!(!plan.contains(word), "{word} in the plan");
    }
}

fn last_plan(rig: &Rig) -> String {
    String::from_utf8(rig.guest.plan_bytes(rig.guest.boots() - 1)).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_start_folder_reaches_the_login_shell_file_and_a_missing_one_is_a_problem_until_it_exists()
 {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;

    // No start folder: new terminals land in the volume root.
    let files = rig.guest.plan_files(rig.guest.boots() - 1);
    let file = pem_of(&files, "/etc/profile.d/03-puddle-start-folder.sh");
    assert!(file.contains("cd '/workspaces/acme'"), "{file}");

    // The setting is checked, saved, and written into the running guest without a restart.
    let boots = rig.guest.boots();
    let set = put_start_folder(&api, "acme", Some("web/src")).await;
    assert_eq!(set.status, 200, "{}", set.body);
    assert_eq!(
        set.json(),
        json!({"start_folder": "web/src", "exists": true})
    );
    assert_eq!(rig.guest.boots(), boots + 1);
    let files = rig.guest.plan_files(rig.guest.boots() - 1);
    let file = pem_of(&files, "/etc/profile.d/03-puddle-start-folder.sh");
    assert!(file.contains("cd '/workspaces/acme/web/src'"), "{file}");
    events
        .until(
            is_event("workspace_repositories_changed", "acme"),
            Duration::from_secs(20),
        )
        .await;
    let read = api.get("/api/workspaces/acme").await;
    assert_eq!(read.json()["start_folder"], "web/src");

    // A folder that is not there opens the root and is a problem; the setting still saves.
    rig.guest.start_folder_missing.store(true, Ordering::SeqCst);
    let set = put_start_folder(&api, "acme", Some("later")).await;
    assert_eq!(
        set.json(),
        json!({"start_folder": "later", "exists": false})
    );
    let (title, detail) = problem_is(&api, "start-folder:acme", true).await.unwrap();
    assert!(title.contains("later"), "{title}");
    assert!(detail.contains("volume root"), "{detail}");
    // It ends when the folder exists (an attach looks again) or the setting goes back to the root.
    rig.guest
        .start_folder_missing
        .store(false, Ordering::SeqCst);
    let attach = api
        .post(
            "/api/workspaces/acme/attach",
            &json!({"mode": "browser"}).to_string(),
        )
        .await;
    assert_eq!(attach.status, 200, "{}", attach.body);
    assert!(problem_is(&api, "start-folder:acme", false).await.is_none());
    rig.guest.start_folder_missing.store(true, Ordering::SeqCst);
    put_start_folder(&api, "acme", Some("later")).await;
    assert!(problem_is(&api, "start-folder:acme", true).await.is_some());
    let reset = put_start_folder(&api, "acme", None).await;
    assert_eq!(reset.json(), json!({"start_folder": null, "exists": null}));
    assert!(problem_is(&api, "start-folder:acme", false).await.is_none());

    // What breaks the rules is refused with the reason; an unknown workspace is a 404.
    for bad in ["../etc", "/abs", "a//b", ".puddle/x", "lost+found"] {
        let refused = put_start_folder(&api, "acme", Some(bad)).await;
        assert_eq!(refused.status, 422, "{bad}: {}", refused.body);
    }
    assert_eq!(put_start_folder(&api, "nope", None).await.status, 404);
    let saved = api.get("/api/workspaces/acme").await;
    assert_eq!(saved.json()["start_folder"], serde_json::Value::Null);
}

/// Stops `name` and starts it again.
async fn stop_and_start(api: &Api, events: &mut support::Events, name: &'static str) {
    for step in ["stop", "start"] {
        let reply = api
            .post(&format!("/api/workspaces/{name}/{step}"), "")
            .await;
        assert_eq!(reply.status, 202, "{}", reply.body);
        events.until(ended(name), Duration::from_secs(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_attach_writes_the_multi_root_file_for_two_checkouts_and_a_file_it_cannot_parse_is_a_problem()
 {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let attach = || async {
        let reply = api
            .post(
                "/api/workspaces/acme/attach",
                &json!({"mode": "browser"}).to_string(),
            )
            .await;
        assert_eq!(reply.status, 200, "{}", reply.body);
    };
    let file = ".puddle/editor/acme.code-workspace";

    // One checkout, or none: the root opens and the plan holds no file.
    attach().await;
    assert!(!last_plan(&rig).contains(file));
    rig.guest
        .put_checkout("api", "https://example.org/acme/api.git");
    attach().await;
    assert!(!last_plan(&rig).contains(file));

    // Two, and a loose file: the plan merges `folders` into the file, one folder per checkout and
    // the volume root last.
    rig.guest
        .put_checkout("web", "https://example.org/acme/web.git");
    rig.guest.other_entries.store(2, Ordering::SeqCst);
    attach().await;
    let plan = last_plan(&rig);
    assert!(plan.contains(file), "{plan}");
    for folder in [
        "/workspaces/acme/api",
        "/workspaces/acme/web",
        "workspace files",
    ] {
        assert!(plan.contains(folder), "{folder}: {plan}");
    }
    // A start keeps the file as the last attach wrote it, so the editor server opens it at once
    // (only a running guest can list the checkouts).
    stop_and_start(&api, &mut events, "acme").await;
    let plan = last_plan(&rig);
    assert!(
        plan.contains(&format!("\npuddle_keep '/workspaces/acme/{file}'\n")),
        "{plan}"
    );
    // A start folder replaces the window: Puddle's lines go again, at an attach and at a start.
    put_start_folder(&api, "acme", Some("api")).await;
    attach().await;
    assert!(!last_plan(&rig).contains(file));
    stop_and_start(&api, &mut events, "acme").await;
    assert!(!last_plan(&rig).contains(file));
    put_start_folder(&api, "acme", None).await;

    // A file the hook cannot parse is left alone and a problem names it, until it parses.
    *rig.guest.boot_says.lock().unwrap() =
        format!("puddle-boot: /workspaces/acme/{file} left as it is: expected a value at line 1\n");
    attach().await;
    let (_, detail) = problem_is(&api, "editor-file:acme", true).await.unwrap();
    assert!(detail.contains("left as it is"), "{detail}");
    attach().await;
    assert!(problem_is(&api, "editor-file:acme", false).await.is_none());

    // Another merged file left first does not hide it: the notice looks at every left file.
    *rig.guest.boot_says.lock().unwrap() = format!(
        "puddle-boot: /home/agent/.docker/config.json left as it is: the file is not UTF-8\n\
         puddle-boot: /workspaces/acme/{file} left as it is: expected a value at line 1\n"
    );
    attach().await;
    let (_, detail) = problem_is(&api, "editor-file:acme", true).await.unwrap();
    assert!(detail.contains(file), "{detail}");
    assert!(!detail.contains("docker"), "{detail}");
}

/// Setting a start folder on a running workspace replaces the multi-root window at once: the plan
/// the guest gets no longer holds the file, and when the next attach cannot list the checkouts the
/// editor still opens the folder, not the file the folder replaced.
#[tokio::test(flavor = "multi_thread")]
async fn a_start_folder_set_on_a_running_workspace_drops_the_multi_root_file_before_any_attach() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create_with_direct_ssh(&api, &mut events, "acme", true).await;
    rig.guest
        .put_checkout("api", "https://example.org/acme/api.git");
    rig.guest
        .put_checkout("web", "https://example.org/acme/web.git");
    let attach = || async {
        let reply = api
            .post(
                "/api/workspaces/acme/attach",
                &json!({"mode": "desktop"}).to_string(),
            )
            .await;
        assert_eq!(reply.json()["opened"], true, "{}", reply.body);
        rig.launcher
            .targets
            .lock()
            .unwrap()
            .last()
            .cloned()
            .unwrap()
    };
    let file = ".puddle/editor/acme.code-workspace";
    assert!(attach().await.is_workspace_file);

    let reply = put_start_folder(&api, "acme", Some("api")).await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    let plan = last_plan(&rig);
    assert!(!plan.contains(file), "{plan}");

    rig.guest.fail_listing.store(true, Ordering::SeqCst);
    let target = attach().await;
    assert_eq!(target.path, "/workspaces/acme/api");
    assert!(!target.is_workspace_file);
    host.shutdown().await;
}

/// A workspace that runs, with nothing else going on in its guest.
async fn quiet_workspace(rig: &Rig, name: &'static str) -> (Host<FakeRuntime>, Api) {
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, name).await;
    after_boot_listing(rig).await;
    (host, api)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_start_folder_whose_list_cannot_be_saved_is_not_changed_and_none_is_taken_after_shutdown_began()
 {
    let rig = Rig::named_ids();
    let (host, api) = quiet_workspace(&rig, "acme").await;
    put_start_folder(&api, "acme", Some("web")).await;
    make_unwritable(&list_path(&rig));

    let refused = put_start_folder(&api, "acme", Some("api")).await;
    assert_eq!(refused.status, 503, "{}", refused.body);
    assert!(
        refused.body.contains("the start folder was not changed"),
        "{}",
        refused.body
    );
    assert_eq!(
        api.get("/api/workspaces/acme").await.json()["start_folder"],
        "web"
    );

    host.workspaces().close();
    let late = put_start_folder(&api, "acme", Some("api")).await;
    assert_eq!(late.status, 503, "{}", late.body);
    assert!(late.body.contains("shutting down"), "{}", late.body);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_guest_that_cannot_look_for_the_start_folder_is_a_problem_until_it_can() {
    let rig = Rig::named_ids();
    let (_host, api) = quiet_workspace(&rig, "acme").await;
    // The login-shell file is written (the first command); the look for the folder is the next.
    rig.runtime.inject(
        Op::Exec,
        Fault::once(disk_error("exec")).only_for("acme").after(1),
    );
    let set = put_start_folder(&api, "acme", Some("web")).await;
    assert_eq!(set.json(), json!({"start_folder": "web", "exists": null}));
    let (title, detail) = problem_is(&api, "start-folder:acme", true).await.unwrap();
    assert!(
        title.contains("could not look for") && title.contains("web"),
        "{title}"
    );
    assert!(detail.contains("no space left"), "{detail}");
    // The next look that works ends it.
    put_start_folder(&api, "acme", Some("web")).await;
    assert!(problem_is(&api, "start-folder:acme", false).await.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_start_folder_the_guest_cannot_be_given_is_a_problem_until_a_change_reaches_it() {
    let rig = Rig::named_ids();
    let (_host, api) = quiet_workspace(&rig, "acme").await;
    rig.runtime
        .inject(Op::Exec, Fault::once(disk_error("exec")).only_for("acme"));
    let set = put_start_folder(&api, "acme", Some("web")).await;
    assert_eq!(set.status, 200, "{}", set.body);
    let (title, detail) = problem_is(&api, "start-folder-file:acme", true)
        .await
        .unwrap();
    assert!(title.contains("start folder"), "{title}");
    assert!(
        detail.contains("no space left") && detail.contains("saved"),
        "{detail}"
    );
    put_start_folder(&api, "acme", Some("web/src")).await;
    assert!(
        problem_is(&api, "start-folder-file:acme", false)
            .await
            .is_none()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_start_folder_changed_while_the_workspace_is_stopped_ends_the_problems_about_the_old_one()
{
    let rig = Rig::named_ids();
    let (_host, api) = quiet_workspace(&rig, "acme").await;
    rig.runtime
        .inject(Op::Exec, Fault::once(disk_error("exec")).only_for("acme"));
    put_start_folder(&api, "acme", Some("web")).await;
    problem_is(&api, "start-folder-file:acme", true).await;
    rig.guest.start_folder_missing.store(true, Ordering::SeqCst);
    put_start_folder(&api, "acme", Some("web/src")).await;
    problem_is(&api, "start-folder:acme", true).await;
    let mut events = api.events().await;
    let stop = api.post("/api/workspaces/acme/stop", "").await;
    assert_eq!(stop.status, 202, "{}", stop.body);
    events.until(ended("acme"), Duration::from_secs(20)).await;
    // Nothing in a stopped workspace can be looked at: the change ends both problems, and the
    // start that comes looks again.
    let set = put_start_folder(&api, "acme", Some("api")).await;
    assert_eq!(set.status, 200, "{}", set.body);
    assert!(problem_is(&api, "start-folder:acme", false).await.is_none());
    assert!(
        problem_is(&api, "start-folder-file:acme", false)
            .await
            .is_none()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_attach_that_cannot_list_or_write_the_editors_file_says_so_until_one_works() {
    let rig = Rig::named_ids();
    let (_host, api) = quiet_workspace(&rig, "acme").await;
    let attach = || async {
        let reply = api
            .post(
                "/api/workspaces/acme/attach",
                &json!({"mode": "browser"}).to_string(),
            )
            .await;
        assert_eq!(reply.status, 200, "{}", reply.body);
    };
    // The listing is the first command of an attach with no start folder, the plan the second.
    for skip in [0, 1] {
        rig.runtime.inject(
            Op::Exec,
            Fault::once(disk_error("exec")).only_for("acme").after(skip),
        );
        attach().await;
        let (title, detail) = problem_is(&api, "editor-file:acme", true).await.unwrap();
        assert!(title.contains("editor's file"), "{title}");
        assert!(detail.contains("no space left"), "{detail}");
        attach().await;
        assert!(problem_is(&api, "editor-file:acme", false).await.is_none());
    }
}
