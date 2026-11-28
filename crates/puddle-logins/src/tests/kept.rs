// SPDX-License-Identifier: AGPL-3.0-or-later
//! What is kept across a restart: the start's load, capture off, the marker, the list of kept
//! logins, forgetting and deleting.

use super::*;
use crate::test_rig::*;

#[tokio::test]
async fn a_restart_gets_the_stand_ins_the_workspace_still_holds() {
    let first = rig();
    let given = claude_login(&first, REAL_ACCESS, REAL_REFRESH).await;

    // The next start: a new registry and new handle over the same credential store.
    let second = rig_on(&first.store, true);
    assert_eq!(second.registry.len(), 0);
    second.logins.load().await;
    assert_eq!(second.registry.len(), 2);
    let access = given["access_token"].as_str().unwrap();
    let refresh = given["refresh_token"].as_str().unwrap();
    assert_eq!(
        second.registry.real_of(access).as_deref(),
        Some(REAL_ACCESS)
    );
    assert_eq!(
        second.registry.real_of(refresh).as_deref(),
        Some(REAL_REFRESH)
    );
    assert_eq!(second.notices.seen(), []);
    assert_eq!(second.logins.kept(), first.logins.kept());
    // A refresh now reuses the stand-ins the workspace holds.
    let renewed = "sk-ant-oat01-CANARY0access0third0GGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGG";
    let again = claude_login(&second, renewed, REAL_REFRESH).await;
    assert_eq!(again["access_token"], given["access_token"]);
    assert_eq!(second.registry.real_of(access).as_deref(), Some(renewed));
    assert_eq!(second.registry.len(), 2);
}

#[tokio::test]
async fn with_capture_off_nothing_is_captured_loaded_or_decrypted_and_a_kept_login_is_said_once() {
    let on = rig();
    claude_login(&on, REAL_ACCESS, REAL_REFRESH).await;

    let off = rig_on(&on.store, false);
    assert!(off.logins.hosts().is_empty());
    assert!(
        off.logins
            .begin(&host("platform.claude.com"), "POST", "/v1/oauth/token")
            .is_none()
    );
    off.logins.load().await;
    assert_eq!(off.registry.len(), 0, "a login kept before is not swapped");
    // The stand-ins in the workspace are refused now, so the service that holds one is named,
    // once even though it holds two tokens; GitHub holds none and is not named.
    assert_eq!(
        off.notices.seen(),
        [("claude".to_owned(), Problem::CaptureOff)]
    );
    // What is kept stays kept: capture on again uses it.
    let again = rig_on(&on.store, true);
    again.logins.load().await;
    assert_eq!(again.registry.len(), 2);
}

#[tokio::test]
async fn with_capture_off_and_nothing_kept_there_is_nothing_to_say() {
    let off = rig_on(&Arc::new(MemoryStore::new()), false);
    off.logins.load().await;
    assert_eq!(off.notices.seen(), []);
}

pub(super) fn marker_in(dir: &tempfile::TempDir) -> Marker {
    Marker::in_file(dir.path().join("alpha.kept"))
}

#[tokio::test]
async fn a_login_kept_with_capture_on_is_said_with_capture_off_without_asking_the_store() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(MemoryStore::new());
    let on = rig_marked(&store, true, marker_in(&dir));
    on.logins.load().await;
    assert_eq!(on.logins.inner_marker_kept(), Some(vec![]));
    claude_login(&on, REAL_ACCESS, REAL_REFRESH).await;
    assert_eq!(
        on.logins.inner_marker_kept(),
        Some(vec!["claude".to_owned()])
    );

    // The store is down, which would make a start without the marker say nothing.
    store.break_it();
    let off = rig_marked(&store, false, marker_in(&dir));
    off.logins.load().await;
    assert_eq!(
        off.notices.seen(),
        [("claude".to_owned(), Problem::CaptureOff)]
    );
}

#[tokio::test]
async fn the_first_start_with_capture_off_asks_the_store_once_and_leaves_a_marker_behind() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(MemoryStore::new());
    claude_login(&rig_on(&store, true), REAL_ACCESS, REAL_REFRESH).await;

    let first = rig_marked(&store, false, marker_in(&dir));
    first.logins.load().await;
    assert_eq!(first.notices.seen().len(), 1);
    assert_eq!(
        first.logins.inner_marker_kept(),
        Some(vec!["claude".to_owned()])
    );

    store.break_it();
    let second = rig_marked(&store, false, marker_in(&dir));
    second.logins.load().await;
    assert_eq!(second.notices.seen(), first.notices.seen());
}

#[tokio::test]
async fn a_store_that_does_not_answer_leaves_the_marker_as_it_was() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(MemoryStore::new());
    store.break_it();
    let off = rig_marked(&store, false, marker_in(&dir));
    off.logins.load().await;
    assert_eq!(off.logins.inner_marker_kept(), None);
    let on = rig_marked(&store, true, marker_in(&dir));
    on.logins.load().await;
    assert_eq!(on.logins.inner_marker_kept(), None);
}

#[tokio::test]
async fn a_marker_that_says_nothing_is_kept_is_believed_and_the_store_is_not_asked() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(MemoryStore::new());
    let off = rig_marked(&store, false, marker_in(&dir));
    off.logins.load().await;
    assert_eq!(off.logins.inner_marker_kept(), Some(vec![]));
    // A login kept without Puddle's marker knowing (by another copy of Puddle, say) is not said:
    // the marker is the answer until capture is on again and reads the store.
    claude_login(&rig_on(&store, true), REAL_ACCESS, REAL_REFRESH).await;
    let again = rig_marked(&store, false, marker_in(&dir));
    again.logins.load().await;
    assert_eq!(again.notices.seen(), []);
    rig_marked(&store, true, marker_in(&dir))
        .logins
        .load()
        .await;
    let later = rig_marked(&store, false, marker_in(&dir));
    later.logins.load().await;
    assert_eq!(later.notices.seen().len(), 1);
}

fn names(list: &crate::KeptList) -> Vec<&'static str> {
    list.logins.iter().map(|login| login.profile).collect()
}

#[tokio::test]
async fn the_list_of_kept_logins_comes_from_the_marker_with_the_time_each_was_noted_and_no_value() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(MemoryStore::new());
    let on = rig_marked(&store, true, marker_in(&dir));
    on.logins.load().await;
    assert_eq!(
        on.logins.kept_list().await,
        crate::KeptList {
            logins: vec![],
            store_answered: true
        }
    );
    claude_login(&on, REAL_ACCESS, REAL_REFRESH).await;

    // The store is down: the marker still answers, without waiting on it.
    store.break_it();
    let off = rig_marked(&store, false, marker_in(&dir));
    let list = off.logins.kept_list().await;
    assert!(list.store_answered);
    assert_eq!(names(&list), ["claude"]);
    assert_eq!(list.logins[0].name, "Claude Code");
    assert!(
        list.logins[0]
            .since_ms
            .is_some_and(|ms| ms > 1_700_000_000_000)
    );
    let shown = format!("{list:?}");
    assert!(!shown.contains("CANARY"), "no token value: {shown}");

    // A later write keeps the time the service was first noted.
    let first = list.logins[0].since_ms;
    store.heal();
    on.logins.load().await;
    assert_eq!(off.logins.kept_list().await.logins[0].since_ms, first);
}

#[tokio::test]
async fn with_no_marker_the_list_asks_the_store_once_and_a_store_that_does_not_answer_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(MemoryStore::new());
    claude_login(&rig_on(&store, true), REAL_ACCESS, REAL_REFRESH).await;

    store.break_it();
    let down = rig_marked(&store, false, marker_in(&dir));
    assert_eq!(
        down.logins.kept_list().await,
        crate::KeptList {
            logins: vec![],
            store_answered: false
        }
    );
    assert_eq!(down.logins.inner_marker_kept(), None);

    store.heal();
    let list = down.logins.kept_list().await;
    assert!(list.store_answered);
    assert_eq!(names(&list), ["claude"]);
    // When it became kept is not known from a store that carries no times.
    assert_eq!(list.logins[0].since_ms, None);
    assert_eq!(
        down.logins.inner_marker_kept(),
        Some(vec!["claude".to_owned()])
    );
}

#[tokio::test]
async fn a_forgotten_login_leaves_the_list_and_a_marker_naming_a_stranger_is_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(MemoryStore::new());
    let on = rig_marked(&store, true, marker_in(&dir));
    on.logins.load().await;
    claude_login(&on, REAL_ACCESS, REAL_REFRESH).await;
    assert_eq!(on.logins.forget("claude").await, Ok(true));
    assert_eq!(names(&on.logins.kept_list().await), Vec::<&str>::new());

    std::fs::write(
        dir.path().join("alpha.kept"),
        "puddle-logins-kept v2\n../../etc/passwd 12\ngithub not-a-number\nclaude 7 8\n",
    )
    .unwrap();
    let list = on.logins.kept_list().await;
    assert_eq!(names(&list), ["claude", "github"]);
    assert_eq!(list.logins[0].since_ms, None);
    assert_eq!(list.logins[1].since_ms, None);
}

#[tokio::test]
async fn forgetting_a_login_ends_the_notice_even_when_the_entries_were_already_gone() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(MemoryStore::new());
    let on = rig_marked(&store, true, marker_in(&dir));
    on.logins.load().await;
    claude_login(&on, REAL_ACCESS, REAL_REFRESH).await;
    // The user cleared the credential store by hand: the marker still says "kept".
    for role in CLAUDE.roles() {
        vault(&on).delete(&CLAUDE, role).unwrap();
    }
    let off = rig_marked(&store, false, marker_in(&dir));
    off.logins.load().await;
    assert_eq!(off.notices.seen().len(), 1, "the marker is believed");

    let off = rig_marked(&store, false, marker_in(&dir));
    assert_eq!(off.logins.forget("claude").await, Ok(true));
    store.break_it();
    let after = rig_marked(&store, false, marker_in(&dir));
    after.logins.load().await;
    assert_eq!(after.notices.seen(), []);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn forgetting_every_login_at_once_leaves_none_in_the_marker() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(MemoryStore::new());
    let ids: std::collections::BTreeSet<String> =
        builtin().iter().map(|p| p.id.to_owned()).collect();
    for _ in 0..100 {
        let rig = rig_marked(&store, false, marker_in(&dir));
        marker_in(&dir).write(&ids);
        let forgets: Vec<_> = builtin()
            .iter()
            .map(|profile| {
                let logins = rig.logins.clone();
                tokio::spawn(async move { logins.forget(profile.id).await })
            })
            .collect();
        for forget in forgets {
            assert_eq!(forget.await.unwrap(), Ok(true));
        }
        assert_eq!(
            rig.logins.inner_marker_kept(),
            Some(Vec::new()),
            "a forget was lost"
        );
    }
}

#[tokio::test]
async fn a_forget_the_store_refuses_keeps_the_marker_so_the_notice_stays() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(MemoryStore::new());
    let on = rig_marked(&store, true, marker_in(&dir));
    on.logins.load().await;
    claude_login(&on, REAL_ACCESS, REAL_REFRESH).await;
    store.break_it();
    assert_eq!(on.logins.forget("claude").await, Err(ForgetError::Store));
    let off = rig_marked(&store, false, marker_in(&dir));
    off.logins.load().await;
    assert_eq!(off.notices.seen().len(), 1);
}

#[tokio::test]
async fn with_capture_off_a_credential_store_that_does_not_answer_says_nothing() {
    let on = rig();
    claude_login(&on, REAL_ACCESS, REAL_REFRESH).await;
    on.store.break_it();
    let off = rig_on(&on.store, false);
    off.logins.load().await;
    assert_eq!(off.registry.len(), 0);
    assert_eq!(off.notices.seen(), []);
}

#[tokio::test]
async fn a_credential_store_that_is_not_there_at_the_start_is_reported_once_per_service_and_read_once()
 {
    let on = rig();
    claude_login(&on, REAL_ACCESS, REAL_REFRESH).await;
    on.store.break_it();
    let next = rig_on(&on.store, true);
    next.logins.load().await;
    assert_eq!(next.registry.len(), 0);
    // Nothing can be kept either, so this is the notice for a login that is not protected.
    assert_eq!(
        next.notices.seen(),
        [
            ("claude".to_owned(), Problem::StoreUnavailable),
            ("github".to_owned(), Problem::StoreUnavailable),
            ("entra".to_owned(), Problem::StoreUnavailable),
            ("openai".to_owned(), Problem::StoreUnavailable),
            ("docker".to_owned(), Problem::StoreUnavailable)
        ]
    );
}

#[tokio::test]
async fn forgetting_a_login_ends_its_stand_ins_and_deletes_its_tokens() {
    let rig = rig();
    claude_login(&rig, REAL_ACCESS, REAL_REFRESH).await;
    assert_eq!(rig.logins.forget("nope").await, Ok(false));
    assert_eq!(rig.logins.forget("claude").await, Ok(true));
    assert_eq!(rig.registry.len(), 0);
    assert_eq!(rig.logins.kept().len(), 0);
    assert!(vault(&rig).read(&CLAUDE, Role::Access).unwrap().is_none());
    assert!(vault(&rig).read(&CLAUDE, Role::Refresh).unwrap().is_none());
    // The next login starts a slot again.
    claude_login(&rig, REAL_ACCESS, REAL_REFRESH).await;
    assert_eq!(rig.registry.len(), 2);
    rig.store.break_it();
    assert_eq!(rig.logins.forget("claude").await, Err(ForgetError::Store));
    assert_eq!(
        rig.registry.len(),
        0,
        "the stand-ins stop working whatever the store does"
    );
}

#[tokio::test]
async fn deleting_the_workspace_deletes_every_token_kept_for_it() {
    let rig = rig();
    claude_login(&rig, REAL_ACCESS, REAL_REFRESH).await;
    let other = {
        let registry = Arc::new(StandIns::new());
        Logins::new(
            WorkspaceId::new("beta").unwrap(),
            true,
            builtin(),
            registry,
            rig.store.clone(),
            rig.notices.clone(),
            Marker::none(),
        )
    };
    let store: Arc<dyn SecretStore> = rig.store.clone();
    other.forget("github").await.unwrap();
    assert_eq!(Logins::delete_workspace(&store, &workspace(), builtin()), 0);
    assert!(vault(&rig).read(&CLAUDE, Role::Access).unwrap().is_none());
    assert!(vault(&rig).read(&CLAUDE, Role::Refresh).unwrap().is_none());
    rig.store.break_it();
    assert_eq!(
        Logins::delete_workspace(&store, &workspace(), builtin()),
        CLAUDE.roles().len()
            + GITHUB.roles().len()
            + crate::profile::ENTRA.slots().len()
            + OPENAI.roles().len()
            + DOCKER.slots().len()
    );
}

#[tokio::test]
async fn a_workspaces_logins_never_reach_another_workspace() {
    let one = rig();
    claude_login(&one, REAL_ACCESS, REAL_REFRESH).await;
    let beta = Logins::new(
        WorkspaceId::new("beta").unwrap(),
        true,
        builtin(),
        Arc::new(StandIns::new()),
        one.store.clone(),
        Arc::new(Notices::default()),
        Marker::none(),
    );
    beta.load().await;
    assert_eq!(beta.kept().len(), 0);
}

#[test]
fn the_hosts_are_every_profiles_endpoints_and_api_hosts_while_capture_is_on() {
    let rig = rig();
    let hosts = rig.logins.hosts();
    for name in [
        "platform.claude.com",
        "console.anthropic.com",
        "api.anthropic.com",
        "github.com",
        "api.github.com",
        "api.individual.githubcopilot.com",
    ] {
        assert!(hosts.contains(&host(name)), "{name}");
    }
    assert!(!hosts.contains(&host("example.com")));
}

#[tokio::test]
async fn the_debug_output_names_no_token_after_a_login_was_kept() {
    let rig = rig();
    let given = claude_login(&rig, REAL_ACCESS, REAL_REFRESH).await;
    assert_eq!(rig.registry.len(), 2, "the login was captured");
    let text = format!("{:?}", rig.logins);
    assert!(text.contains("Logins"), "{text}");
    for secret in [
        REAL_ACCESS,
        REAL_REFRESH,
        given["access_token"].as_str().unwrap(),
        given["refresh_token"].as_str().unwrap(),
    ] {
        assert!(!text.contains(secret), "a token is printed in {text}");
    }
}

#[tokio::test]
async fn a_saved_entry_that_is_not_one_of_ours_is_reported_as_unreadable_and_the_rest_still_loads()
{
    let first = rig();
    claude_login(&first, REAL_ACCESS, REAL_REFRESH).await;
    // The refresh slot's entry is damaged.
    let id = crate::vault::slot_id("alpha", "claude", Role::Refresh).unwrap();
    first
        .store
        .set(&id, &puddle_secrets::Secret::new("not json".into()))
        .unwrap();
    let next = rig_on(&first.store, true);
    next.logins.load().await;
    assert_eq!(next.registry.len(), 1, "the access slot is back");
    assert_eq!(
        next.notices.seen(),
        [("claude".to_owned(), Problem::Unreadable)]
    );
}
