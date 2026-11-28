// SPDX-License-Identifier: AGPL-3.0-or-later
//! Git identities, credentials, kept logins and repository lists.

use super::*;

#[tokio::test]
async fn a_kept_login_is_forgotten_until_the_script_makes_the_store_refuse() {
    let run = start("lived-in").await;
    let step = |body: Value| {
        let run = &run;
        async move { run.control("POST", "/control/step", Some(&body)).await }
    };
    let forget = |service: &'static str| {
        let run = &run;
        async move {
            run.api(
                "DELETE",
                &format!("/api/workspaces/web-shop/logins/{service}"),
                None,
            )
            .await
        }
    };
    // The name the notice carries works too, as the real host accepts it.
    assert_eq!(forget("Claude%20Code").await.status, 204);

    assert_eq!(
        step(json!({"do": "login_store_refuses", "refuses": true}))
            .await
            .status,
        204
    );
    let refused = forget("claude").await;
    assert_eq!(refused.status, 503, "{}", refused.body);
    assert!(
        refused.json()["message"]
            .as_str()
            .is_some_and(|m| m.contains("unlock it or start it and ask again")),
        "{}",
        refused.body
    );

    step(json!({"do": "login_store_refuses", "refuses": false})).await;
    assert_eq!(forget("claude").await.status, 204);
}

#[tokio::test]
async fn a_workspaces_kept_logins_are_listed_as_the_script_sets_them_and_forgetting_one_ends_its_row()
 {
    let run = start("lived-in").await;
    let none = run
        .api("GET", "/api/workspaces/web-shop/logins", None)
        .await;
    assert_eq!(
        none.json(),
        json!({"logins": [], "store_readable": true}),
        "{}",
        none.body
    );
    let set = json!({"do": "kept_logins", "workspace": "web-shop", "logins": [
        {"service": "claude", "name": "Claude Code", "since": 1_761_480_000_000_u64},
        {"service": "github", "name": "GitHub"}
    ]});
    assert_eq!(
        run.control("POST", "/control/step", Some(&set))
            .await
            .status,
        204
    );
    let listed = run
        .api("GET", "/api/workspaces/web-shop/logins", None)
        .await;
    assert_eq!(listed.json()["logins"][0]["since"], 1_761_480_000_000_u64);
    assert_eq!(listed.json()["logins"][1]["since"], Value::Null);
    assert_eq!(
        run.api("DELETE", "/api/workspaces/web-shop/logins/claude", None)
            .await
            .status,
        204
    );
    let after = run
        .api("GET", "/api/workspaces/web-shop/logins", None)
        .await;
    assert_eq!(after.json()["logins"].as_array().map(Vec::len), Some(1));

    let unreadable = json!({"do": "kept_logins", "workspace": "web-shop", "logins": [],
        "store_readable": false});
    run.control("POST", "/control/step", Some(&unreadable))
        .await;
    let down = run
        .api("GET", "/api/workspaces/web-shop/logins", None)
        .await;
    assert_eq!(down.json()["store_readable"], false);
    let bad = json!({"do": "kept_logins", "workspace": "not a name!", "logins": []});
    assert_ne!(
        run.control("POST", "/control/step", Some(&bad))
            .await
            .status,
        204
    );
}

#[tokio::test]
async fn the_git_identities_scenario_seeds_identities_tables_and_what_the_credentials_service_answers()
 {
    let run = start("git-identities").await;
    let identities = run.get("/api/identities").await.json();
    let labels: Vec<_> = identities["identities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            (
                i["label"].as_str().unwrap(),
                i["is_default"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(labels, [("Work", true), ("Personal", false)]);

    let shop = run.get("/api/workspaces/web-shop/git").await.json();
    assert_eq!(shop["identities"][0]["label"], "Work");
    let rows: Vec<_> = shop["repos"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["repo"].as_str().unwrap(),
                r["pull"].as_bool().unwrap(),
                r["push"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [("web-shop", true, true), ("design-tokens", true, false)]
    );
    let docs = run.get("/api/workspaces/docs-site/git").await.json();
    assert_eq!(docs["identities"].as_array().unwrap().len(), 2);
    assert_eq!(docs["only_push_listed"], false);
    let tools = run.get("/api/workspaces/data-tools/git").await.json();
    assert_eq!(tools["identities"], json!([]));
    assert_eq!(tools["repos"], json!([]));

    let found = run.get("/api/credentials/found").await.json();
    assert_eq!(found["accounts"].as_array().unwrap().len(), 4);
    let signed_out = json!({"source": {"kind": "git_credential", "host": "dev.azure.com", "path": "contoso", "username": null}});
    let check = async |body: &Value| {
        run.api("POST", "/api/credentials/check", Some(body))
            .await
            .json()
    };
    assert_eq!(check(&signed_out).await["readable"], false);

    // A step makes it readable, as a finished sign-in would; another replaces what is found.
    let step = run
        .control(
            "POST",
            "/control/step",
            Some(&json!({"do": "credential_readable", "readable": true, "source": signed_out["source"]})),
        )
        .await;
    assert_eq!(step.status, 204, "{}", step.body);
    assert_eq!(check(&signed_out).await["readable"], true);
    let step = run
        .control(
            "POST",
            "/control/step",
            Some(&json!({"do": "credentials_found", "accounts": []})),
        )
        .await;
    assert_eq!(step.status, 204, "{}", step.body);
    assert_eq!(
        run.get("/api/credentials/found").await.json()["accounts"],
        json!([])
    );

    // Sign-in needed and refused Git access arrive as events (the scripts emit them).
    let mut stream = run.events().await;
    for script in ["sign_in_needed", "push_denied", "pull_denied"] {
        let ran = run
            .control("POST", &format!("/control/script/{script}"), None)
            .await;
        assert_eq!(ran.status, 204, "{script}");
    }
    let seen = read_until(&mut stream, |s| {
        s.contains("credential_sign_in_needed") && s.matches("git_access_denied").count() >= 2
    })
    .await;
    assert!(
        seen.contains(r#""access":"push""#) && seen.contains(r#""access":"pull""#),
        "{seen}"
    );
}

#[tokio::test]
async fn the_azure_cli_scenario_answers_a_check_with_the_step_and_starts_no_sign_in() {
    let run = start("azure-cli").await;
    let source = json!({"kind": "az_cli", "tenant": "contoso.onmicrosoft.com", "org": "contoso"});
    let listed = run.get("/api/identities").await.json();
    assert_eq!(listed["identities"][0]["credentials"][0]["source"], source);
    let checked = run
        .api(
            "POST",
            "/api/credentials/check",
            Some(&json!({"source": source})),
        )
        .await;
    assert_eq!(checked.status, 200, "{}", checked.body);
    let result = checked.json();
    assert_eq!(result["readable"], false);
    assert_eq!(result["needs_sign_in"], false);
    assert!(
        result["fix_step"]
            .as_str()
            .unwrap()
            .contains("az login --tenant contoso.onmicrosoft.com")
    );
    run.fixture.shutdown().await;
}

#[tokio::test]
async fn the_azure_cli_missing_scenario_names_the_install_step_and_the_wrong_tenant_step() {
    let run = start("azure-cli-missing").await;
    let check = |tenant: &'static str, org: &'static str| {
        let run = &run;
        async move {
            let reply = run
                .api(
                    "POST",
                    "/api/credentials/check",
                    Some(&json!({"source": {"kind": "az_cli", "tenant": tenant, "org": org}})),
                )
                .await;
            assert_eq!(reply.status, 200, "{}", reply.body);
            reply.json()
        }
    };
    let missing = check("fabrikam.com", "fabrikam").await;
    assert_eq!(missing["readable"], false);
    assert_eq!(missing["needs_sign_in"], false);
    assert!(
        missing["fix_step"]
            .as_str()
            .unwrap()
            .contains("Install the Azure CLI"),
        "{missing}"
    );
    let zero = check("00000000-0000-0000-0000-000000000000", "fabrikam").await;
    assert_eq!(zero["needs_sign_in"], false);
    assert!(
        zero["fix_step"]
            .as_str()
            .unwrap()
            .contains("is not in tenant 00000000-0000-0000-0000-000000000000"),
        "{zero}"
    );
    run.fixture.shutdown().await;
}

#[tokio::test]
async fn a_read_step_makes_the_identities_carry_what_the_last_read_came_to() {
    let run = start("git-identities").await;
    let last = |list: &Value, label: usize, credential: usize| {
        list["identities"][label]["credentials"][credential]["last_check"].clone()
    };
    let before = run.get("/api/identities").await.json();
    assert!(last(&before, 1, 0).is_null(), "nothing read yet");

    let mut stream = run.events().await;
    let ran = run
        .control("POST", "/control/script/pushes_read_credentials", None)
        .await;
    assert_eq!(ran.status, 204);
    read_until(&mut stream, |s| s.contains("identities_changed")).await;

    let after = run.get("/api/identities").await.json();
    let fine = last(&after, 1, 0);
    assert_eq!(
        (fine["readable"].clone(), fine["by"].clone()),
        (serde_json::json!(true), serde_json::json!("injection"))
    );
    let broken = last(&after, 0, 1);
    assert_eq!(
        (broken["needs_sign_in"].clone(), broken["by"].clone()),
        (
            serde_json::json!(true),
            serde_json::json!("repository_list")
        )
    );
    assert!(
        last(&after, 0, 0).is_null(),
        "the third credential was never read"
    );

    // A test is a use too.
    let step = json!({"do": "credential_read", "by": "test", "readable": true,
        "source": {"kind": "gh", "host": "github.com", "account": "tijs-work"}});
    let ran = run.control("POST", "/control/step", Some(&step)).await;
    assert_eq!(ran.status, 204, "{}", ran.body);
    let tested = run.get("/api/identities").await.json();
    assert_eq!(last(&tested, 0, 0)["by"], "test");
}

#[tokio::test]
async fn the_fixture_answers_the_repository_endpoints_with_nothing_listed() {
    let run = start("git-identities").await;
    let listing = run.get("/api/repos").await;
    assert_eq!(listing.status, 200, "{}", listing.body);
    assert_eq!(
        listing.json(),
        json!({"sources": [], "repos": [], "total": 0, "offset": 0, "limit": 100})
    );
    let profile = run
        .api(
            "POST",
            "/api/credentials/profile",
            Some(&json!({"source": {"kind": "gh", "host": "github.com", "account": "me"}})),
        )
        .await;
    assert_eq!(profile.status, 200, "{}", profile.body);
    assert_eq!(profile.json()["problem"]["code"], "unreachable");
}

#[tokio::test]
async fn seeded_profiles_answer_by_credential_and_keep_notes_and_problems() {
    let run = start("git-identities").await;
    let ask = |source: Value| {
        let run = &run;
        async move {
            let answer = run
                .api(
                    "POST",
                    "/api/credentials/profile",
                    Some(&json!({ "source": source })),
                )
                .await;
            assert_eq!(answer.status, 200, "{}", answer.body);
            answer.json()
        }
    };
    let demo = ask(json!({"kind": "gh", "host": "github.com", "account": "tijs-demo"})).await;
    assert_eq!(demo["name"], "Tijs Demo");
    assert_eq!(demo["email"], "4242+tijs-demo@users.noreply.github.com");
    assert_eq!(demo["organisations"], json!(["demo-org", "demo-labs"]));
    assert_eq!(demo["notes"][0]["code"], "sso_partial");
    assert_eq!(demo["problem"], Value::Null);
    let old = ask(json!({"kind": "gh", "host": "github.com", "account": "tijs-old"})).await;
    assert_eq!(old["problem"]["code"], "token_rejected");
    assert_eq!(old["problem"]["needs_sign_in"], true);
    let azure = ask(json!({"kind": "git_credential", "host": "dev.azure.com", "path": "contoso", "username": null})).await;
    assert_eq!(azure["organisations"], json!(["contoso"]));
    assert_eq!(azure["notes"][0]["code"], "author_unavailable");
}

#[tokio::test]
async fn a_kept_pasted_token_is_answered_by_its_value_and_listed_until_forgotten() {
    let run = start("git-identities").await;
    let keep = |token: &'static str| {
        let run = &run;
        async move {
            let kept = run
                .api(
                    "POST",
                    "/api/credentials/stored",
                    Some(&json!({"host": "github.com", "org": null, "token": token})),
                )
                .await;
            assert_eq!(kept.status, 201, "{}", kept.body);
            kept.json()["source"].clone()
        }
    };
    let ask = |source: Value| {
        let run = &run;
        async move {
            let answer = run
                .api(
                    "POST",
                    "/api/credentials/profile",
                    Some(&json!({ "source": source })),
                )
                .await;
            assert_eq!(answer.status, 200, "{}", answer.body);
            answer.json()
        }
    };
    let seeded = keep("ghp_e2ePastedLookup0123456789").await;
    let other = keep("ghp_someOtherToken0123456789").await;
    let known = ask(seeded.clone()).await;
    assert_eq!(known["account"], "tijs-pasted");
    assert_eq!(known["organisations"], json!(["pasted-org"]));
    assert_eq!(ask(other).await["problem"]["code"], "unreachable");
    assert_eq!(
        run.state().await["kept_tokens"].as_array().unwrap().len(),
        2
    );
    let forgotten = run
        .api(
            "DELETE",
            &format!("/api/credentials/stored/{}", seeded["id"].as_str().unwrap()),
            None,
        )
        .await;
    assert_eq!(forgotten.status, 204, "{}", forgotten.body);
    assert_eq!(
        run.state().await["kept_tokens"].as_array().unwrap().len(),
        1
    );
}

#[tokio::test]
async fn a_profile_seed_names_a_source_or_a_token_not_both_and_not_neither() {
    for seed in [
        json!({}),
        json!({"token": "t", "source": {"kind": "gh", "host": "github.com", "account": "x"}}),
    ] {
        let scenario: Scenario = serde_json::from_value(json!({"profiles": [seed]})).unwrap();
        let err = Fixture::start(FixtureOptions {
            port: 0,
            connection_file: None,
            scenario,
        })
        .await
        .err()
        .unwrap();
        assert!(err.contains("`source` or a `token`"), "{err}");
    }
}

#[tokio::test]
async fn a_profile_for_a_credential_that_cannot_be_named_fails_to_start() {
    let scenario: Scenario = serde_json::from_value(json!({
        "profiles": [{"source": {"kind": "gh", "host": "not a host", "account": "x"}}],
    }))
    .unwrap();
    let err = Fixture::start(FixtureOptions {
        port: 0,
        connection_file: None,
        scenario,
    })
    .await
    .err()
    .unwrap();
    assert!(err.contains("host"), "{err}");
}

#[tokio::test]
async fn the_credentials_service_can_report_accounts_from_every_listing_and_listings_that_could_not_run()
 {
    let scenario: Scenario = serde_json::from_value(json!({
        "credentials": {
            "found": [
                {"via": "gcm_github", "host": "github.com", "account": "me", "org": null, "signed_in": true},
                {"via": "gcm_azure_repos", "host": "dev.azure.com", "account": "me@example.com", "org": "acme", "signed_in": true}
            ],
            "missing": ["gh", "gcm_github"]
        }
    }))
    .unwrap();
    let run = start_scenario(scenario).await;
    let found = run.get("/api/credentials/found").await.json();
    let via: Vec<_> = found["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["via"].as_str().unwrap())
        .collect();
    assert_eq!(via, ["gcm_github", "gcm_azure_repos"]);
    let problems: Vec<_> = found["problems"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| (p["via"].as_str().unwrap(), p["message"].as_str().unwrap()))
        .collect();
    assert_eq!(
        problems,
        [
            ("gh", "gh is not installed or not on PATH"),
            ("gcm_github", "git is not installed or not on PATH")
        ]
    );
}

#[tokio::test]
async fn a_scenario_whose_workspace_names_an_identity_that_is_not_there_fails_to_start() {
    let bad: Scenario = serde_json::from_value(json!({
        "workspaces": [{"name": "a", "repositories": [{"url": "https://github.com/x/y.git"}], "git": {"identities": ["Ghost"]}}]
    }))
    .unwrap();
    let err = Fixture::start(FixtureOptions {
        port: 0,
        connection_file: None,
        scenario: bad,
    })
    .await
    .err()
    .unwrap();
    assert!(err.contains("Ghost"), "{err}");
}

fn every_kind_of_list() -> Scenario {
    let problems = [
        "not_signed_in",
        "source_unavailable",
        "token_rejected",
        "forbidden",
        "not_found",
        "rate_limited",
        "unreachable",
        "bad_answer",
        "unsupported",
        "organisation_needed",
        "wrong_target",
    ];
    let notes = [
        "sso_partial",
        "organisations_may_be_hidden",
        "fine_grained_token",
        "truncated",
        "author_unavailable",
        "organisations_unavailable",
        "host_limit_reached",
    ];
    let states = ["ok", "stale", "failed", "unavailable"];
    let roles = ["admin", "maintain", "write", "triage", "read", "unknown"];
    let visibilities = ["public", "private", "internal", "unknown"];
    let lists: Vec<Value> = problems
        .iter()
        .enumerate()
        .map(|(i, code)| {
            json!({
                "identity": "Me",
                "credential": i,
                "host": "github.com",
                "state": states[i % states.len()],
                "refreshed_ago_ms": 1000,
                "retry_in_ms": 5000,
                "problem": {"code": code, "message": "why", "needs_sign_in": i == 0},
                "notes": notes.iter().map(|n| json!({"code": n, "message": "n"})).collect::<Vec<_>>(),
                "repos": roles.iter().zip(visibilities.iter().cycle()).map(|(role, vis)| json!({
                    "owner": "acme", "name": format!("r{i}-{role}"), "role": role, "visibility": vis,
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    serde_json::from_value(json!({
        "identities": [{
            "label": "Me",
            "author": {"name": "Me", "email": "me@example.com"},
            "credentials": [],
        }],
        "repo_lists": lists,
    }))
    .unwrap()
}

#[tokio::test]
async fn seeded_repository_lists_keep_every_state_note_problem_role_and_visibility() {
    let run = start_scenario(every_kind_of_list()).await;
    let listing = run.get("/api/repos").await;
    assert_eq!(listing.status, 200, "{}", listing.body);
    let body = listing.json();
    let sources = body["sources"].as_array().unwrap();
    assert_eq!(sources.len(), 11);
    assert_eq!(sources[2]["problem"]["code"], "token_rejected");
    assert_eq!(sources[0]["problem"]["needs_sign_in"], true);
    assert_eq!(sources[1]["state"], "stale");
    assert_eq!(sources[0]["notes"].as_array().unwrap().len(), 7);
    assert_eq!(
        sources[0]["retry_at"],
        sources[0]["refreshed_at"].as_u64().unwrap() + 6000
    );
    assert_eq!(body["total"], 66);
}

#[tokio::test]
async fn an_azure_devops_project_with_a_space_gets_an_address_with_the_space_escaped() {
    let scenario: Scenario = serde_json::from_value(json!({
        "identities": [{"label": "Me", "author": {"name": "Me", "email": "me@example.com"}, "credentials": []}],
        "repo_lists": [{
            "identity": "Me", "host": "dev.azure.com", "organisation": "contoso",
            "repos": [{"owner": "contoso", "project": "Shop Floor", "name": "scanner"}],
        }],
    }))
    .unwrap();
    let run = start_scenario(scenario).await;
    let body = run.get("/api/repos").await.json();
    assert_eq!(
        body["repos"][0]["url"],
        "https://dev.azure.com/contoso/Shop%20Floor/_git/scanner"
    );
    assert_eq!(body["repos"][0]["full_name"], "contoso/Shop Floor/scanner");
}

#[tokio::test]
async fn a_list_for_an_identity_that_is_not_there_fails_to_start() {
    let scenario: Scenario = serde_json::from_value(json!({
        "repo_lists": [{"identity": "Nobody", "host": "github.com"}],
    }))
    .unwrap();
    let err = Fixture::start(FixtureOptions {
        port: 0,
        connection_file: None,
        scenario,
    })
    .await
    .err()
    .unwrap();
    assert!(err.contains("Nobody"), "{err}");
}

#[tokio::test]
async fn repository_lists_can_be_held_released_and_replaced_by_steps() {
    let run = start("repo-lists").await;
    let step = |body: Value| {
        let run = &run;
        async move { run.control("POST", "/control/step", Some(&body)).await }
    };
    assert_eq!(step(json!({"do": "hold_repos"})).await.status, 204);
    let held =
        tokio::time::timeout(Duration::from_millis(300), run.get("/api/repos?identity=2")).await;
    assert!(held.is_err(), "the answer should wait while held");
    assert_eq!(step(json!({"do": "release_repos"})).await.status, 204);
    let listing = run.get("/api/repos?identity=2").await.json();
    assert_eq!(listing["total"], 3);
    let replaced = step(json!({"do": "repo_lists", "lists": [
        {"identity": "Personal", "host": "github.com", "repos": [{"owner": "me", "name": "only"}]}
    ]}))
    .await;
    assert_eq!(replaced.status, 204, "{}", replaced.body);
    let listing = run.get("/api/repos?identity=2").await.json();
    assert_eq!(listing["total"], 1);
    let bad =
        step(json!({"do": "repo_lists", "lists": [{"identity": "Nobody", "host": "x"}]})).await;
    assert_eq!(bad.status, 422, "{}", bad.body);
}
