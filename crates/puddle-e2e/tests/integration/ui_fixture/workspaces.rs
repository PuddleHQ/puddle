// SPDX-License-Identifier: AGPL-3.0-or-later
//! Seeded workspaces, checkouts, scripted workspace operations, backups and restores.

use super::*;

#[tokio::test]
async fn a_seeded_workspace_with_an_id_keeps_it_apart_from_its_name() {
    let mut scenario = load_scenario("empty").unwrap();
    scenario.workspaces = serde_json::from_value(json!([
        {"name": "Shop One", "id": "shop-1"},
        {"name": "plain"}
    ]))
    .unwrap();
    scenario.requests = serde_json::from_value(json!([
        {"workspace": "shop-1", "host": "a.example.org"}
    ]))
    .unwrap();
    let run = start_scenario(scenario).await;
    let list = run.get("/api/workspaces").await.json();
    let pairs: Vec<_> = list["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| (w["id"].as_str().unwrap(), w["name"].as_str().unwrap()))
        .collect();
    assert!(pairs.contains(&("shop-1", "Shop One")), "{pairs:?}");
    assert!(pairs.contains(&("plain", "plain")), "{pairs:?}");
    assert_eq!(
        run.get("/api/workspaces/shop-1/delete-check").await.status,
        200
    );
}

#[tokio::test]
async fn a_built_in_scenario_carries_workspaces_whose_ids_differ_from_their_names() {
    let run = start("ids-differ").await;
    let list = run.get("/api/workspaces").await.json();
    let rows = list["workspaces"].as_array().unwrap();
    assert_eq!(rows.len(), 9);
    for w in rows {
        assert_ne!(w["id"], w["name"], "{w}");
    }
}

#[tokio::test]
async fn seeded_workspaces_are_listed_with_what_deleting_them_would_lose() {
    let run = start("lived-in").await;
    let list = run.get("/api/workspaces").await.json();
    let rows = list["workspaces"].as_array().unwrap();
    let names: Vec<_> = rows.iter().map(|w| w["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["data-tools", "docs-site", "web-shop"]);
    let status = |name: &str| {
        rows.iter().find(|w| w["name"] == name).unwrap()["status"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    assert_eq!(status("web-shop"), "running");
    assert_eq!(status("docs-site"), "stopped");
    assert_eq!(status("data-tools"), "created");
    let docs = rows.iter().find(|w| w["name"] == "docs-site").unwrap();
    assert_eq!(docs["memory_mib"], 4096);
    assert_eq!(docs["direct_ssh"], false);
    assert_eq!(docs["first_connect_notice_due"], false);

    let check = run
        .get("/api/workspaces/docs-site/delete-check")
        .await
        .json();
    assert_eq!(check["clean"], false);
    assert_eq!(
        check["repos"][0]["unpushed"]["items"][0],
        "3f2a9c1 Rewrite the intro"
    );
    assert_eq!(check["other"]["items"][0], "scratch");
    let clean = run
        .get("/api/workspaces/web-shop/delete-check")
        .await
        .json();
    assert_eq!(clean["clean"], true);

    let empty = start("empty").await;
    assert_eq!(
        empty.get("/api/workspaces").await.json(),
        json!({"workspaces": []})
    );
}

#[tokio::test]
async fn seeded_checkouts_are_listed_for_a_running_workspace_and_a_stopped_one_has_none() {
    let run = start("unmanaged-checkouts").await;
    let list = run.get("/api/workspaces/dev/checkouts").await.json();
    assert_eq!(list["running"], true, "{list}");
    let dirs: Vec<_> = list["checkouts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["directory"].as_str().unwrap())
        .collect();
    assert_eq!(
        dirs,
        [
            "api",
            "broken",
            "hostile",
            "no-remote",
            "over-ssh",
            "personal",
            "scratch"
        ]
    );
    let by = |dir: &str| {
        list["checkouts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["directory"] == dir)
            .unwrap()
            .clone()
    };
    assert_eq!(by("api")["entry"]["url"], "https://github.com/acme/api.git");
    assert_eq!(by("api")["dirty"], true);
    assert_eq!(by("scratch")["entry"], Value::Null);
    assert_eq!(by("scratch")["manage"]["offered"], true);
    assert_eq!(by("over-ssh")["manage"]["offered"], false);
    assert_eq!(list["missing"], json!(["docs"]));

    let parked = run.get("/api/workspaces/parked/checkouts").await.json();
    assert_eq!(parked["running"], false, "{parked}");
    assert_eq!(parked["checkouts"], json!([]));
    assert!(parked["problem"].is_string(), "{parked}");
}

#[tokio::test]
async fn every_remote_note_of_a_seeded_checkout_says_why_it_cannot_be_managed() {
    let mut scenario = load_scenario("unmanaged-checkouts").unwrap();
    let extra: Vec<puddle_e2e::ui_fixture::CheckoutSeed> = serde_json::from_value(json!([
        {"directory": "stripped", "remote_note": "credentials_removed",
         "remote_url": "https://github.com/acme/stripped.git"},
        {"directory": "plain-http", "remote_note": "not_https"}
    ]))
    .unwrap();
    scenario.workspaces[0].checkouts.extend(extra);
    let run = start_scenario(scenario).await;
    let list = run.get("/api/workspaces/dev/checkouts").await.json();
    let by = |dir: &str| {
        list["checkouts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["directory"] == dir)
            .unwrap()
            .clone()
    };
    assert_eq!(by("stripped")["remote_note"], "credentials_removed");
    assert_eq!(by("plain-http")["remote_note"], "not_https");
    assert_eq!(by("plain-http")["manage"]["offered"], false);
}

#[tokio::test]
async fn scripted_steps_hold_and_fail_workspace_operations() {
    let run = start("empty").await;
    let mut stream = run.events().await;
    let step = |body: Value| {
        let run = &run;
        async move { run.control("POST", "/control/step", Some(&body)).await }
    };
    assert_eq!(step(json!({"do": "hold_workspaces"})).await.status, 204);
    let created = run
        .api(
            "POST",
            "/api/workspaces",
            Some(&json!({"name": "demo", "repositories": [{"url": "https://github.com/acme/demo.git"}]})),
        )
        .await;
    assert_eq!(created.status, 202, "{}", created.body);
    assert_eq!(
        run.get("/api/workspaces/demo").await.json()["busy"],
        "creating"
    );
    assert_eq!(step(json!({"do": "release_workspaces"})).await.status, 204);
    read_until(&mut stream, |s| s.contains("\"step\":\"done\"")).await;
    assert_eq!(
        run.get("/api/workspaces/demo").await.json()["busy"],
        Value::Null
    );

    let fail =
        json!({"do": "fail_workspace", "operation": "start", "reason": "the VM did not boot"});
    assert_eq!(step(fail).await.status, 204);
    let started = run
        .api("POST", "/api/workspaces/demo/start", Some(&json!({})))
        .await;
    assert_eq!(started.status, 202);
    let seen = read_until(&mut stream, |s| s.contains("did not boot")).await;
    assert!(seen.contains("\"step\":\"failed\""), "{seen}");
    assert_eq!(
        run.get("/api/workspaces/demo").await.json()["status"],
        "crashed"
    );
    for operation in ["create", "stop", "reclaim", "delete"] {
        let body = json!({"do": "fail_workspace", "operation": operation, "reason": "x"});
        assert_eq!(step(body).await.status, 204, "{operation}");
    }
}

#[tokio::test]
async fn scripted_steps_raise_and_end_background_problems() {
    let run = start("lived-in").await;
    let mut stream = run.events().await;
    let step = |body: Value| {
        let run = &run;
        async move { run.control("POST", "/control/step", Some(&body)).await }
    };
    assert_eq!(run.get("/api/problems").await.json()["problems"], json!([]));
    let raise = json!({
        "do": "problem",
        "key": "clone:web-shop:api",
        "workspace": "web-shop",
        "title": "Could not clone https://github.com/acme/api.git",
        "detail": "fatal: repository not found"
    });
    assert_eq!(step(raise).await.status, 204);
    read_until(&mut stream, |s| s.contains("problems_changed")).await;
    let listed = run.get("/api/problems").await.json();
    assert_eq!(listed["problems"][0]["key"], "clone:web-shop:api");
    assert_eq!(listed["problems"][0]["workspace"], "web-shop");
    assert_eq!(
        listed["problems"][0]["detail"],
        "fatal: repository not found"
    );
    let about_puddle = json!({"do": "problem", "key": "k", "title": "t", "detail": "d"});
    assert_eq!(step(about_puddle).await.status, 204);
    assert_eq!(
        run.get("/api/problems").await.json()["problems"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        step(json!({"do": "clear_problem", "key": "clone:web-shop:api"}))
            .await
            .status,
        204
    );
    assert_eq!(
        run.get("/api/problems").await.json()["problems"][0]["key"],
        "k"
    );
    let bad =
        json!({"do": "problem", "key": "k", "workspace": "Bad_Name", "title": "t", "detail": "d"});
    assert_eq!(step(bad).await.status, 422);
}

#[tokio::test]
async fn a_scenario_with_a_bad_workspace_fails_to_start() {
    let cases = [
        (
            json!({"name": "A_b", "repositories": [{"url": "https://x.test/a"}]}),
            "A_b",
        ),
        (
            json!({"name": "a", "repositories": [{"url": "git@x.test:a/b"}]}),
            "reads SSH addresses",
        ),
        (
            json!({"name": "a", "repositories": [{"url": "https://x.test/a"}], "memory_mib": 1}),
            "memory",
        ),
        (
            json!({"name": "a", "repositories": [{"url": "https://x.test/a"}], "image": "bad image"}),
            "image",
        ),
    ];
    for (workspace, wants) in cases {
        let scenario: Scenario =
            serde_json::from_value(json!({"workspaces": [workspace]})).unwrap();
        let err = Fixture::start(FixtureOptions {
            port: 0,
            connection_file: None,
            scenario,
        })
        .await
        .err()
        .unwrap();
        assert!(err.contains(wants), "{wants}: {err}");
    }
    assert!(
        serde_json::from_value::<Scenario>(
            json!({"workspaces": [{"name": "a", "repositories": [{"url": "https://x.test/a"}], "typo": 1}]})
        )
        .is_err()
    );
}

#[tokio::test]
async fn an_orphan_disk_with_an_address_that_is_not_a_repository_is_refused_with_the_reason() {
    let running = start(built_in_names()[0]).await;
    let step: Step =
        serde_json::from_value(json!({"do": "orphan_volume", "id": "lost", "remote": "a b"}))
            .unwrap();
    let err = running.fixture.apply(&step).await.unwrap_err();
    assert!(err.contains("space"), "{err}");
}

#[tokio::test]
async fn an_ssh_agent_ask_step_puts_one_open_question_for_the_workspace() {
    let run = start("empty").await;
    let body = json!({"do": "ssh_agent_ask", "workspace": "demo", "host": "git.example.org"});
    assert_eq!(
        run.control("POST", "/control/step", Some(&body))
            .await
            .status,
        204
    );
    let asks = run.get("/api/ssh-agent-asks").await.json();
    assert_eq!(asks["asks"].as_array().map(Vec::len), Some(1), "{asks}");
    assert_eq!(asks["asks"][0]["host"], "git.example.org");
    assert_eq!(asks["asks"][0]["workspace"], "demo");
}

#[tokio::test]
async fn the_backup_download_works_and_a_step_makes_it_fail_and_recover() {
    let run = start("default").await;
    let made = run.api("POST", "/api/backup", None).await;
    assert_eq!(made.status, 200, "{}", made.body);
    assert!(made.body.contains("fake backup"), "{}", made.body);

    let failing = json!({"do": "backup_fails", "reason": "the disk is full"});
    let set = run.control("POST", "/control/step", Some(&failing)).await;
    assert_eq!(set.status, 204, "{}", set.body);
    let failed = run.api("POST", "/api/backup", None).await;
    assert_eq!(failed.status, 503, "{}", failed.body);
    assert!(failed.body.contains("the disk is full"), "{}", failed.body);

    let clear = json!({"do": "backup_fails"});
    run.control("POST", "/control/step", Some(&clear)).await;
    assert_eq!(run.api("POST", "/api/backup", None).await.status, 200);
}

#[tokio::test]
async fn a_restore_previews_applies_and_lists_safety_copies_through_steps() {
    let run = start("default").await;
    // A body that is not a backup is refused with the reason, and nothing is applied.
    let refused = run.api("POST", "/api/restore/preview", None).await;
    assert_eq!(refused.status, 422, "{}", refused.body);

    let steps = [
        json!({"do": "restore_preview", "missing_disks": ["gone"], "secrets": ["TOKEN"]}),
        json!({"do": "safety_copies", "names": ["20231114T221320Z"]}),
        json!({"do": "restore_result", "applied": true, "message": "Restored."}),
    ];
    for step in steps {
        let set = run.control("POST", "/control/step", Some(&step)).await;
        assert_eq!(set.status, 204, "{}", set.body);
    }
    let copies = run.api("GET", "/api/safety-copies", None).await.json();
    assert_eq!(copies["copies"][0]["name"], "20231114T221320Z", "{copies}");
    let result = run.api("GET", "/api/restore/result", None).await.json();
    assert_eq!(result["result"]["message"], "Restored.", "{result}");
    let applied = run
        .api(
            "POST",
            "/api/restore/apply",
            Some(&json!({"staging_id": "0".repeat(32), "mode": "add"})),
        )
        .await;
    assert_eq!(applied.status, 200, "{}", applied.body);

    let failing = json!({"do": "restore_apply_fails", "reason": "the disk is full"});
    run.control("POST", "/control/step", Some(&failing)).await;
    let failed = run
        .api(
            "POST",
            "/api/restore/apply",
            Some(&json!({"staging_id": "0".repeat(32), "mode": "add"})),
        )
        .await;
    assert_eq!(failed.status, 503, "{}", failed.body);
}
