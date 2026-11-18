// SPDX-License-Identifier: AGPL-3.0-or-later
//! Company proxy sign-ins and audit records.

use super::*;

/// A host whose discovery answers a company proxy at `proxy` (a PAC says so), signing in with
/// the Basic password `password`.
async fn host_behind_proxy(
    rig: &Rig,
    proxy: &puddle_upstream::FakeProxy,
    password: &str,
) -> Host<FakeRuntime> {
    let mut config = rig.config();
    config.upstream.basic = Some(puddle_upstream::Credentials::new("svc", password));
    host_behind_proxy_with(rig, proxy, config).await
}

/// Like [`host_behind_proxy`], with the Basic sign-in left to the stored ones.
async fn host_behind_proxy_without_a_configured_password(
    rig: &Rig,
    proxy: &puddle_upstream::FakeProxy,
) -> Host<FakeRuntime> {
    host_behind_proxy_with(rig, proxy, rig.config()).await
}

async fn host_behind_proxy_with(
    rig: &Rig,
    proxy: &puddle_upstream::FakeProxy,
    config: HostConfig,
) -> Host<FakeRuntime> {
    let os = FakeOs::new(ProxyConfig {
        pac_url: Some("http://pac.corp/p.pac".into()),
        ..ProxyConfig::default()
    });
    let hops = vec![puddle_upstream::Hop::Proxy(proxy.proxy_addr())];
    os.set_pac(move |_| Ok(hops.clone()));
    let prepared = prepare(config, &FakePlatform::new(&rig.log)).unwrap();
    let mut options = hermetic_options();
    options.secret_store = Some(rig.secrets.clone());
    options.dev_cert = Some(rig.dev_cert.clone() as Arc<dyn DevCertSource>);
    options.discovery = Some(Discovery::new(os, puddle_upstream::Config::default()));
    Host::start(prepared, &FakeFactory::new(&rig.runtime, &rig.log), options)
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sign_in_to_the_company_proxy_shows_in_the_served_network_health_report() {
    use puddle_upstream::{Behaviour, FakeProxy};
    let rig = Rig::new();
    let right = fixture::password("right");
    // The proxy asks for Basic once, then answers the CONNECT with "no such host": nothing
    // leaves the test.
    let proxy = FakeProxy::start(Behaviour::Basic {
        user: "svc".into(),
        password: right.clone(),
    })
    .await;
    let host = host_behind_proxy(&rig, &proxy, &right).await;
    let api = api(&host);

    let before = api.get("/api/network-health").await.json();
    assert_eq!(before["sign_in"]["attempts"], json!([]), "{before}");

    add_github_identity(&api, "ghp_unused_token").await;
    let listing = api.get("/api/repos").await.json();
    assert_eq!(listing["sources"][0]["state"], "failed", "{listing}");

    let reply = api.get("/api/network-health").await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    fixture::assert_absent("the network-health reply", &reply.body, &right);
    let report = reply.json();
    assert!(
        report["sign_in"]["methods"]
            .as_array()
            .unwrap()
            .contains(&json!("basic")),
        "{report}"
    );
    let attempts = report["sign_in"]["attempts"].as_array().unwrap();
    assert_eq!(attempts.len(), 1, "{report}");
    assert_eq!(attempts[0]["proxy"], proxy.proxy_addr().to_string());
    assert_eq!(attempts[0]["scheme"], "Basic");
    assert_eq!(attempts[0]["result"], "signed_in");
    assert!(attempts[0]["at"].as_u64().unwrap() > 0, "{report}");
    // The proxy received the Basic credentials on a CONNECT for the listing's host. Which requests
    // came before is not asserted: a platform that can sign in without being asked (Windows
    // Negotiate) sends its own token first.
    let seen = proxy.seen();
    let expected = format!("Basic {}", base64(format!("svc:{right}").as_bytes()));
    let signed_in = seen.iter().any(|request| {
        request.method == "CONNECT"
            && request.target == "api.github.com:443"
            && request.proxy_authorization.as_deref() == Some(expected.as_str())
    });
    assert!(
        signed_in,
        "the proxy never saw the Basic sign-in on a CONNECT"
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_sign_in_shows_as_failed_without_the_password() {
    use puddle_upstream::{Behaviour, FakeProxy};
    let rig = Rig::new();
    let wrong = fixture::password("wrong");
    let proxy = FakeProxy::start(Behaviour::Basic {
        user: "svc".into(),
        password: fixture::password("right"),
    })
    .await;
    let host = host_behind_proxy(&rig, &proxy, &wrong).await;
    let api = api(&host);

    add_github_identity(&api, "ghp_unused_token").await;
    let listing = api.get("/api/repos").await.json();
    assert_eq!(listing["sources"][0]["state"], "failed", "{listing}");

    let reply = api.get("/api/network-health").await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    fixture::assert_absent("the network-health reply", &reply.body, &wrong);
    let report = reply.json();
    let attempts = report["sign_in"]["attempts"].as_array().unwrap();
    assert_eq!(attempts.len(), 1, "{report}");
    assert_eq!(attempts[0]["proxy"], proxy.proxy_addr().to_string());
    assert_eq!(attempts[0]["scheme"], "Basic");
    assert_eq!(attempts[0]["result"], "failed");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn listing_the_repositories_of_an_identity_is_in_the_activity_log_as_puddles_own_request() {
    use puddle_upstream::{Behaviour, FakeProxy, Hop};
    let rig = Rig::new();
    // The company proxy refuses every connection, so no request leaves the test.
    let proxy = FakeProxy::start(Behaviour::Refuse(502)).await;
    let os = FakeOs::new(ProxyConfig {
        pac_url: Some("http://pac.corp/p.pac".into()),
        ..ProxyConfig::default()
    });
    let hops = vec![Hop::Proxy(proxy.proxy_addr())];
    os.set_pac(move |_| Ok(hops.clone()));
    let prepared = prepare(rig.config(), &FakePlatform::new(&rig.log)).unwrap();
    let mut options = hermetic_options();
    options.secret_store = Some(rig.secrets.clone());
    options.dev_cert = Some(rig.dev_cert.clone() as Arc<dyn DevCertSource>);
    options.discovery = Some(Discovery::new(os, puddle_upstream::Config::default()));
    let host = Host::start(prepared, &FakeFactory::new(&rig.runtime, &rig.log), options)
        .await
        .unwrap();
    let api = api(&host);
    add_github_identity(&api, "ghp_CANARY_listing_token").await;

    let listing = api.get("/api/repos").await.json();
    assert_eq!(listing["sources"][0]["state"], "failed", "{listing}");
    assert_eq!(listing["sources"][0]["problem"]["code"], "unreachable");

    let records: Vec<serde_json::Value> = host
        .store()
        .audit_lines(0, 1000)
        .unwrap()
        .into_iter()
        .filter_map(|(_, line)| serde_json::from_str(&line).ok())
        .filter(|v: &serde_json::Value| v["type"] == "connection" && v["host"] == "api.github.com")
        .collect();
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0]["origin"], "puddle", "{records:?}");
    assert!(records[0]["workspace"].is_null(), "{records:?}");
    assert_eq!(records[0]["port"], 443, "{records:?}");
    assert_eq!(
        records[0]["reason"], "puddle_request_failed:unreachable",
        "{records:?}"
    );
    assert!(!records[0].to_string().contains("CANARY"), "{records:?}");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_image_pull_that_cannot_be_cleaned_up_names_what_is_left() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    rig.runtime.inject(
        Op::PullImage,
        Fault::once(puddle_compute::ComputeError::ImagePull {
            image: "x".into(),
            reason: "offline".into(),
        }),
    );
    rig.runtime
        .inject(Op::RemoveVolume, Fault::always(disk_error("remove volume")));
    api.post("/api/workspaces", &new_workspace("acme")).await;
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    let detail = end["detail"].as_str().unwrap();
    assert!(detail.contains("offline"), "the cause: {detail}");
    assert!(
        detail.contains("remove the new volume"),
        "what is left: {detail}"
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_workspace_that_cannot_make_its_folder_is_taken_down_again_and_the_leftovers_are_named() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    rig.guest.fail_layout.store(true, Ordering::SeqCst);
    // Taking the machine down fails too: it may still run, and its record stays.
    rig.runtime
        .inject(Op::Stop, Fault::once(disk_error("stop")).only_for("acme"));
    rig.runtime.inject(
        Op::Remove,
        Fault::once(disk_error("remove")).only_for("acme"),
    );
    api.post("/api/workspaces", &new_workspace("acme")).await;
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "failed", "{end}");
    let detail = end["detail"].as_str().unwrap();
    assert!(
        detail.contains("read-only file system"),
        "the cause: {detail}"
    );
    assert!(detail.contains("may still be running"), "{detail}");
    assert!(detail.contains("sandbox acme was not removed"), "{detail}");
    assert!(detail.contains("next start"), "the way out: {detail}");

    // Without the faults, the same failure on a start of an existing workspace ends crashed.
    rig.guest.fail_layout.store(false, Ordering::SeqCst);
    let reply = api.post("/api/workspaces", &new_workspace("other")).await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    events.until(ended("other"), Duration::from_secs(20)).await;
    api.post("/api/workspaces/other/stop", "").await;
    events.until(ended("other"), Duration::from_secs(20)).await;
    rig.guest.fail_layout.store(true, Ordering::SeqCst);
    api.post("/api/workspaces/other/start", "").await;
    let end = events.until(ended("other"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "failed", "{end}");
    assert!(
        end["detail"].as_str().unwrap().contains("read-only"),
        "{end}"
    );
    assert_eq!(
        status_of(&api.get("/api/workspaces/other").await),
        "crashed"
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn locks_that_could_not_be_removed_one_by_one_are_listed_in_the_problem() {
    let rig = Rig::named_ids();
    *rig.guest.locks_answer.lock().unwrap() =
        Some((0, "E\tapi/.git/index.lock\tPermission denied\nD\n"));
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let problems = host.problems().list();
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(
        problems[0]
            .detail
            .contains("api/.git/index.lock: Permission denied"),
        "{problems:?}"
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn git_files_repaired_at_boot_are_audited_and_told_in_plain_words() {
    let rig = Rig::named_ids();
    *rig.guest.locks_answer.lock().unwrap() = Some((
        0,
        "R\tapi/.git/HEAD\tHEAD was zero-filled, rewritten: branch main\n\
         R\tapi/.git/index\tindex was damaged, rebuilt from HEAD (staged changes are not kept)\n\
         P\tweb/.git/HEAD\tHEAD is damaged and the last commit in logs/HEAD (abc) is not in the repository\nD\n",
    ));
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let problems = problems_of(&api).await;
    assert_eq!(problems.len(), 1, "{problems:?}");
    let (key, workspace, title, detail) = &problems[0];
    assert_eq!(key, "git-repair:acme");
    assert_eq!(workspace.as_deref(), Some("acme"));
    assert!(title.contains("could not repair"), "{title}");
    assert!(
        detail.contains("api/.git/HEAD: HEAD was zero-filled"),
        "{detail}"
    );
    assert!(
        detail.contains("web/.git/HEAD: HEAD is damaged"),
        "{detail}"
    );
    assert!(
        detail.contains("add it again"),
        "the staged-changes notice: {detail}"
    );
    assert!(detail.contains("fixed by hand"), "{detail}");

    let ops = workspace_ops("workspace=acme", &api).await;
    let repaired: Vec<_> = ops.iter().filter(|r| r["op"] == "git_repaired").collect();
    assert_eq!(repaired.len(), 2, "one audit line per repair: {ops:?}");
    assert_eq!(repaired[0]["actor"], "system", "{ops:?}");
    assert_eq!(repaired[0]["workspace_id"], "acme", "{ops:?}");
    assert!(
        repaired[0]["detail"]
            .as_str()
            .unwrap()
            .starts_with("api/.git/HEAD:"),
        "{ops:?}"
    );

    // A boot that repaired something and left nothing damaged says only what was repaired.
    *rig.guest.locks_answer.lock().unwrap() = Some((
        0,
        "R\tapi/.git/HEAD\tHEAD was zero-filled, rewritten: branch main\nD\n",
    ));
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let problems = problems_of(&api).await;
    assert_eq!(problems.len(), 1, "{problems:?}");
    let (_, _, title, detail) = &problems[0];
    assert!(title.contains("repaired git files in acme"), "{title}");
    assert!(!detail.contains("fixed by hand"), "{detail}");

    // A boot that finds nothing withdraws the notice and writes no new audit line.
    *rig.guest.locks_answer.lock().unwrap() = None;
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(problems_of(&api).await, []);
    let again = workspace_ops("workspace=acme", &api).await;
    assert_eq!(
        again.iter().filter(|r| r["op"] == "git_repaired").count(),
        3
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stored_proxy_sign_in_answers_the_proxy_and_is_there_after_a_restart() {
    use puddle_upstream::{Behaviour, FakeProxy};
    let rig = Rig::new();
    let stored = fixture::password("stored");
    let proxy = FakeProxy::start(Behaviour::Basic {
        user: "svc".into(),
        password: stored.clone(),
    })
    .await;
    let host = host_behind_proxy_without_a_configured_password(&rig, &proxy).await;
    proxy_sign_ins_loaded(&host, 0).await;
    host.proxy_passwords()
        .set(&proxy.proxy_addr(), "svc", &stored)
        .unwrap();
    let api = api(&host);
    add_github_identity(&api, "ghp_unused_token").await;
    let _ = api.get("/api/repos").await;
    let seen = proxy.seen();
    let expected = format!("Basic {}", base64(format!("svc:{stored}").as_bytes()));
    assert!(
        seen.iter().any(|request| request.method == "CONNECT"
            && request.target == "api.github.com:443"
            && request.proxy_authorization.as_deref() == Some(expected.as_str())),
        "the proxy never saw the stored sign-in on a CONNECT"
    );
    host.shutdown().await;
    drop(host);

    // A second run finds it in the credential store, and nothing but the store holds it.
    let again = host_behind_proxy_without_a_configured_password(&rig, &proxy).await;
    proxy_sign_ins_loaded(&again, 1).await;
    assert_eq!(
        again.proxy_passwords().proxies(),
        [(proxy.proxy_addr(), "svc".to_owned())]
    );
    let settings = std::fs::read_dir(rig.dir.path())
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .map(|e| std::fs::read_to_string(e.path()).unwrap_or_default())
        .collect::<String>();
    fixture::assert_absent("the settings files", &settings, &stored);
    again.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_proxy_sign_in_entered_through_the_api_answers_the_proxy_shows_in_the_report_and_goes_when_removed()
 {
    use puddle_upstream::{Behaviour, FakeProxy};
    let rig = Rig::new();
    let stored = fixture::password("entered");
    let proxy = FakeProxy::start(Behaviour::Basic {
        user: "svc".into(),
        password: stored.clone(),
    })
    .await;
    let host = host_behind_proxy_without_a_configured_password(&rig, &proxy).await;
    proxy_sign_ins_loaded(&host, 0).await;
    let api = api(&host);
    let address = proxy.proxy_addr().to_string();

    let put = api
        .put(
            "/api/proxy-sign-ins",
            &json!({"proxy": address, "user": "svc", "password": stored}).to_string(),
        )
        .await;
    assert_eq!(put.status, 200, "{}", put.body);
    fixture::assert_absent("the sign-in reply", &put.body, &stored);
    assert_eq!(
        put.json()["sign_ins"],
        json!([{"proxy": address, "user": "svc"}])
    );

    add_github_identity(&api, "ghp_unused_token").await;
    let _ = api.get("/api/repos").await;
    let expected = format!("Basic {}", base64(format!("svc:{stored}").as_bytes()));
    assert!(
        proxy
            .seen()
            .iter()
            .any(|request| request.method == "CONNECT"
                && request.proxy_authorization.as_deref() == Some(expected.as_str())),
        "the proxy never saw the entered sign-in"
    );
    let report = api.get("/api/network-health").await;
    fixture::assert_absent("the network-health reply", &report.body, &stored);
    assert_eq!(
        report.json()["sign_in"]["stored"],
        json!([{"proxy": address, "user": "svc"}])
    );
    assert_eq!(
        report.json()["sign_in"]["attempts"][0]["result"],
        "signed_in"
    );

    // A removal stops the answers at once.
    let gone = api
        .delete(&format!("/api/proxy-sign-ins/{address}"), None)
        .await;
    assert_eq!(gone.status, 204, "{}", gone.body);
    assert!(host.proxy_passwords().proxies().is_empty());
    let before = proxy.seen().len();
    let _ = api.post("/api/repos/refresh", "{}").await;
    let after = proxy.seen();
    assert!(after.len() > before, "the listing made no new request");
    assert!(
        after[before..]
            .iter()
            .all(|request| request.proxy_authorization.as_deref() != Some(expected.as_str())),
        "the removed sign-in was still sent"
    );
    let report = api.get("/api/network-health").await.json();
    assert_eq!(report["sign_in"]["stored"], json!([]));
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_proxy_that_turns_the_sign_in_down_is_one_event_and_a_failed_attempt() {
    use puddle_upstream::{Behaviour, FakeProxy};
    let rig = Rig::new();
    let wrong = fixture::canary("wrong");
    let proxy = FakeProxy::start(Behaviour::Basic {
        user: "svc".into(),
        password: fixture::password("right"),
    })
    .await;
    let host = host_behind_proxy(&rig, &proxy, &wrong).await;
    let mut events = host.events().subscribe();
    let api = api(&host);
    add_github_identity(&api, "ghp_unused_token").await;
    // Two listings, each retrying the same wrong password: one episode.
    let _ = api.get("/api/repos").await;
    let _ = api.get("/api/repos").await;

    // The host forwards the failure from its own task, so the event can land after the replies:
    // wait for the first one, then give a second (which must not come) a moment to show.
    let sign_in_failure = |event| match event {
        Event::ProxySignInFailed { proxy, scheme } => Some((proxy, scheme)),
        _ => None,
    };
    let first = next_event_matching(
        "the sign-in failure on the event stream",
        Duration::from_secs(10),
        &mut events,
        sign_in_failure,
    )
    .await;
    let failures = vec![first];
    no_event_matching(
        "a second sign-in failure in the same episode",
        Duration::from_millis(500),
        &mut events,
        sign_in_failure,
    )
    .await;
    assert_eq!(
        failures,
        [(proxy.proxy_addr().to_string(), Some("Basic".to_owned()))]
    );
    let report = api.get("/api/network-health").await;
    fixture::assert_absent("the network-health reply", &report.body, &wrong);
    assert_eq!(report.json()["sign_in"]["attempts"][0]["result"], "failed");
    host.shutdown().await;
}

/// A guest connection counts as open from the moment the proxy took its stream, and stops counting
/// when the stream is gone: the signal a reader waits on to know the audit holds every record.
#[tokio::test(flavor = "multi_thread")]
async fn a_guest_connection_counts_as_open_until_it_is_gone() {
    use futures_util::StreamExt as _;
    use puddle_agent_proto::tokio_yamux::Session;
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    assert_eq!(host.open_guest_streams(), 0);

    let sandbox = WorkspaceId::new("acme").unwrap().sandbox_name();
    let routes = rig.runtime.routes(&sandbox);
    let conn = puddle_ipc::connect(&routes.first().unwrap().host)
        .await
        .unwrap();
    let mut session = Session::new_client(conn, puddle_agent_proto::yamux::client_config());
    let mut control = session.control();
    let driver = tokio::spawn(async move { while let Some(Ok(_)) = session.next().await {} });
    let mut stream = control.open_stream().await.unwrap();
    // A head that never completes: the proxy holds the stream while it waits for the rest.
    tokio::io::AsyncWriteExt::write_all(&mut stream, b"CONNECT slow.example.test:443 HTTP/1.1\r\n")
        .await
        .unwrap();
    eventually(
        "the proxy serves the stream",
        Duration::from_secs(10),
        || match host.open_guest_streams() {
            1 => Ok(()),
            open => Err(format!("{open} open")),
        },
    )
    .await;

    drop(stream);
    driver.abort();
    eventually(
        "the stream is gone",
        Duration::from_secs(10),
        || match host.open_guest_streams() {
            0 => Ok(()),
            open => Err(format!("{open} open")),
        },
    )
    .await;
}
