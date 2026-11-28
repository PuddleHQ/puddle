// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn a_workspace_warns_about_a_foreign_credential_by_its_setting_read_each_time() {
    let store = store();
    let ws = workspace("alpha");
    let (injection, _) = injection(&store);
    // Without settings to read the warning is on.
    assert!(injection.foreign_warning(&ws).is_on());
    let repo = Arc::new(puddle_api::MemorySettings::default());
    let injection = injection.with_settings(repo.clone());
    let warning = injection.foreign_warning(&ws);
    assert!(warning.is_on());
    puddle_api::SettingsRepo::save_workspace(
        repo.as_ref(),
        &ws,
        serde_json::json!({"overrides": {"warn_foreign_credentials": false}}),
    )
    .unwrap();
    assert!(!warning.is_on(), "a change applies to the next request");
    assert!(injection.foreign_warning(&workspace("beta")).is_on());
}
#[tokio::test]
async fn a_sandbox_gets_its_own_ca_and_decrypts_what_its_identities_name() {
    let store = store();
    let (injection, terminations) = injection(&store);
    let (a, b) = (workspace("alpha"), workspace("beta"));
    attach(&store, &a, "ada", "github.com");

    let began_a = injection.begin(&a, false).await.unwrap();
    let began_b = injection.begin(&b, false).await.unwrap();
    assert_ne!(began_a.certificate, began_b.certificate);
    assert!(decrypts(&terminations, &a, "github.com"));
    assert!(!decrypts(&terminations, &a, "gitlab.com"));
    // beta has no identity: a CA (the guest trusts it from boot) and nothing decrypted.
    assert!(!decrypts(&terminations, &b, "github.com"));
    assert_eq!(injection.running_workspaces(), [a.clone(), b]);
    assert_eq!(began_a.git.identities.len(), 1);
}
#[tokio::test]
async fn refresh_applies_a_new_host_to_the_running_workspace_with_the_same_ca() {
    let store = store();
    let (injection, terminations) = injection(&store);
    let ws = workspace("alpha");
    injection.begin(&ws, false).await.unwrap();
    let ca_before = terminations
        .termination(&ws)
        .unwrap()
        .ca()
        .certificate()
        .clone();
    assert!(!decrypts(&terminations, &ws, "gitlab.com"));

    attach(&store, &ws, "ada", "gitlab.com");
    assert!(injection.resync(&ws).unwrap().is_some());
    assert!(decrypts(&terminations, &ws, "gitlab.com"));
    // The CA the guest was given at boot is the one that still signs.
    let ca_after = terminations
        .termination(&ws)
        .unwrap()
        .ca()
        .certificate()
        .clone();
    assert_eq!(ca_before, ca_after);
}
#[tokio::test]
async fn the_rules_on_method_and_path_follow_the_store_and_their_hosts_are_decrypted() {
    let store = store();
    let (injection, terminations) = injection(&store);
    let ws = workspace("alpha");
    let host = |h: &str| Host::parse_normalised(h).unwrap();
    let add = |workspace: Option<&WorkspaceId>, host: &str, path: &str| {
        store
            .as_user(puddle_store::UserId::OWNER)
            .add_request_rule(&puddle_store::NewRequestRule {
                workspace: workspace.cloned(),
                host: puddle_types::HostPattern::parse_normalised(host).unwrap(),
                methods: puddle_types::Methods::parse(["DELETE"]).unwrap(),
                path: puddle_types::PathPattern::parse(path).unwrap(),
                created_by: puddle_store::Actor::Api,
                precedence: puddle_store::Precedence::DEFAULT,
            })
            .unwrap()
    };
    let delete_refused = |t: &Terminations, h: &str, path: &str| {
        let headers: Vec<String> = Vec::new();
        let view = RequestView::new("DELETE", path, &headers);
        t.termination(&ws)
            .unwrap()
            .request_rules()
            .refusing(&host(h), &view)
    };
    add(None, "global.example", "/a");
    add(Some(&workspace("beta")), "beta.example", "/a");
    let own = add(Some(&ws), "own.example", "/b/**");
    injection.begin(&ws, false).await.unwrap();
    assert!(delete_refused(&terminations, "global.example", "/a").is_some());
    assert_eq!(
        delete_refused(&terminations, "own.example", "/b/c").map(|id| id.0),
        Some(own.id)
    );
    assert!(
        delete_refused(&terminations, "beta.example", "/a").is_none(),
        "another workspace's rule does not apply"
    );
    assert!(delete_refused(&terminations, "own.example", "/other").is_none());
    let decrypted = terminations.termination(&ws).unwrap();
    assert!(decrypted.set().contains(&host("own.example")));
    assert!(decrypted.set().contains(&host("global.example")));
    assert!(!decrypted.set().contains(&host("beta.example")));

    store
        .as_user(puddle_store::UserId::OWNER)
        .delete_request_rule(own.id, puddle_store::Actor::Api)
        .unwrap();
    add(Some(&ws), "new.example", "/n");
    injection.resync(&ws).unwrap();
    assert!(delete_refused(&terminations, "own.example", "/b/c").is_none());
    assert!(delete_refused(&terminations, "new.example", "/n").is_some());
}
#[tokio::test]
async fn a_workspace_on_the_curated_mode_decrypts_unlisted_hosts_and_passes_the_curated_ones() {
    let store = store();
    let (injection, terminations) = injection(&store);
    let ws = workspace("alpha");
    store
        .set_workspace_mode(&ws, Some(InterceptionMode::AllExceptCurated))
        .unwrap();
    injection.begin(&ws, false).await.unwrap();
    let interception = terminations
        .termination(&ws)
        .unwrap()
        .interception()
        .clone();
    let host = |h: &str| Host::parse_normalised(h).unwrap();
    assert!(interception.is_decrypting_all());
    assert_eq!(interception.skipped(&host("unlisted.example")), None);
    for name in CURATED_PASS_THROUGH.iter().filter(|n| !n.starts_with("*.")) {
        assert_eq!(
            interception.skipped(&host(name)),
            Some(PassThroughCode::Curated),
            "{name}"
        );
    }
}
#[tokio::test]
async fn a_stored_never_decrypt_name_that_does_not_parse_fails_the_start_and_names_the_row() {
    let store = store();
    let (injection, _terminations) = injection(&store);
    let ws = workspace("alpha");
    store
        .set_workspace_mode(&ws, Some(InterceptionMode::All))
        .unwrap();
    // The API refuses such a name; a row written by another version or by hand must not
    // leave a list that silently lacks it.
    puddle_store::testing::break_database(
        &store,
        "INSERT INTO never_decrypt (workspace_id, pattern) VALUES ('alpha', 'not a host')",
    );
    let Err(err) = injection.begin(&ws, false).await else {
        panic!("a corrupt never-decrypt row must fail the start");
    };
    assert!(
        err.to_string().contains("not a host"),
        "the error names the stored name: {err}"
    );
}
#[tokio::test]
async fn the_switch_and_the_never_decrypt_names_apply_to_the_running_workspace_and_keep_what_was_learned()
 {
    let store = store();
    let (injection, terminations) = injection(&store);
    let ws = workspace("alpha");
    let host = |h: &str| Host::parse_normalised(h).unwrap();
    let mode = |t: &Terminations| {
        let interception = t.termination(&ws).unwrap().interception().clone();
        (
            interception.is_decrypting_all(),
            interception.skipped(&host("bank.example")),
            interception.skipped(&host("global.example")),
        )
    };
    store
        .set_workspace_mode(&ws, Some(InterceptionMode::All))
        .unwrap();
    store
        .set_never_decrypt(
            &ws,
            &[puddle_types::HostPattern::parse_normalised("bank.example").unwrap()],
        )
        .unwrap();
    store
        .set_global_never_decrypt(&[
            puddle_types::HostPattern::parse_normalised("global.example").unwrap(),
        ])
        .unwrap();
    injection.begin(&ws, false).await.unwrap();
    assert_eq!(
        mode(&terminations),
        (
            true,
            Some(PassThroughCode::NeverDecrypt),
            Some(PassThroughCode::NeverDecrypt)
        ),
        "read at the start"
    );

    let learned = Arc::clone(
        terminations
            .termination(&ws)
            .unwrap()
            .interception()
            .learned(),
    );
    learned.learn(&host("pinned.example"));
    store
        .set_workspace_mode(&ws, Some(InterceptionMode::KnownOnly))
        .unwrap();
    assert!(injection.resync(&ws).unwrap().is_some());
    assert_eq!(mode(&terminations), (false, None, None), "switched off");
    store
        .set_workspace_mode(&ws, Some(InterceptionMode::All))
        .unwrap();
    store.set_never_decrypt(&ws, &[]).unwrap();
    injection.resync(&ws).unwrap();
    assert_eq!(
        mode(&terminations),
        (true, None, Some(PassThroughCode::NeverDecrypt))
    );
    let after = terminations.termination(&ws).unwrap();
    assert!(
        after
            .interception()
            .learned()
            .contains(&host("pinned.example")),
        "a change to the settings does not forget what the tools refused"
    );
    assert!(Arc::ptr_eq(after.interception().learned(), &learned));
}
#[tokio::test]
async fn a_stopped_sandbox_has_no_ca_and_decrypts_nothing() {
    let store = store();
    let (injection, terminations) = injection(&store);
    let ws = workspace("alpha");
    attach(&store, &ws, "ada", "github.com");
    injection.begin(&ws, false).await.unwrap();
    injection.end(&ws);
    assert!(terminations.termination(&ws).is_none());
    assert_eq!(injection.running_workspaces(), []);
    // A change to a workspace that is not running touches nothing.
    assert!(injection.resync(&ws).unwrap().is_none());
    assert!(terminations.termination(&ws).is_none());
    injection.end(&ws);
}
#[tokio::test]
async fn a_new_start_replaces_the_ca_of_the_earlier_one() {
    let store = store();
    let (injection, terminations) = injection(&store);
    let ws = workspace("alpha");
    let first = injection.begin(&ws, false).await.unwrap().certificate;
    let second = injection.begin(&ws, false).await.unwrap().certificate;
    assert_ne!(first, second);
    assert_eq!(
        terminations.termination(&ws).unwrap().ca().certificate(),
        &second
    );
}
#[tokio::test]
async fn the_longest_workspace_id_still_gives_a_valid_ca_name() {
    let store = store();
    let (injection, _) = injection(&store);
    let ws = workspace(&"a".repeat(WorkspaceId::MAX_LEN));
    puddle_store::testing::own(&store, &ws);
    assert!(injection.begin(&ws, false).await.is_ok());
}
