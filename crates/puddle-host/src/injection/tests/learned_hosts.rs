// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[tokio::test]
async fn a_host_a_tool_refused_is_announced_with_its_count_and_listed_with_its_history() {
    use puddle_services::learned::LearnedService;

    let store = store();
    let (injection, terminations, _, events) = injection_with_events(&store);
    let service = crate::learned::HostLearned::new(Arc::clone(&terminations), Arc::clone(&store));
    let ws = workspace("alpha");
    let host = |h: &str| Host::parse_normalised(h).unwrap();
    assert_eq!(service.list(&handle(&ws)).len(), 0, "nothing learned yet");
    assert!(!service.forget(&handle(&ws), "pinned.example"));
    assert_eq!(service.clear(&handle(&ws)), 0);
    assert!(!service.discard(&handle(&ws), "pinned.example"));

    injection.begin(&ws, false).await.unwrap();
    let learned = Arc::clone(
        terminations
            .termination(&ws)
            .unwrap()
            .interception()
            .learned(),
    );
    learned.learn(&host("pinned.example"));
    learned.learn(&host("pinned.example"));
    learned.learn(&host("other.example"));
    let learned_event = |h: &str, count: u32| Event::PassThroughLearned {
        workspace: ws.clone(),
        host: h.to_owned(),
        count,
    };
    assert_eq!(
        told(&events),
        [
            learned_event("pinned.example", 1),
            learned_event("other.example", 1)
        ],
        "each new host once, renewing says nothing"
    );

    let listed = service.list(&handle(&ws));
    assert_eq!(listed.len(), 2);
    assert!(
        listed.iter().all(|h| h.passed_through
            && h.count == 1
            && h.first_rejected_at == h.last_rejected_at
            && h.expires_in_secs
                .is_some_and(|s| (43_199..=43_200).contains(&s))),
        "{listed:?}: the time is the built-in twelve hours, rounded up"
    );
    assert!(!service.forget(&handle(&ws), "never.example"));
    assert!(!service.forget(&handle(&ws), "not a host"));
    assert!(service.forget(&handle(&ws), "pinned.example"));
    assert!(!learned.contains(&host("pinned.example")));
    assert!(
        !service.forget(&handle(&ws), "pinned.example"),
        "no longer passed through"
    );

    let after = service.list(&handle(&ws));
    assert_eq!(
        after
            .iter()
            .map(|h| (h.host.as_str(), h.passed_through))
            .collect::<Vec<_>>(),
        [("other.example", true), ("pinned.example", false)],
        "the host decrypted again stays listed with its history, after the passed through one"
    );

    // It refuses again: the count rises and the user is told again.
    learned.learn(&host("pinned.example"));
    assert_eq!(
        told(&events).last(),
        Some(&learned_event("pinned.example", 2)),
        "the count rises with each refusal after the host was decrypted again"
    );
    assert_eq!(
        service
            .list(&handle(&ws))
            .iter()
            .find(|h| h.host == "pinned.example")
            .map(|h| h.count),
        Some(2)
    );

    assert_eq!(service.clear(&handle(&ws)), 2);
    assert_eq!(
        service.list(&handle(&ws)).len(),
        2,
        "clearing keeps the history"
    );
    assert!(service.discard(&handle(&ws), "pinned.example"));
    assert!(!service.discard(&handle(&ws), "pinned.example"));
    assert_eq!(service.list(&handle(&ws)).len(), 1);
    assert!(!learned.contains(&host("other.example")));
    assert!(service.discard(&handle(&ws), "other.example"));
    assert_eq!(service.list(&handle(&ws)).len(), 0);
}
#[tokio::test]
async fn a_store_that_cannot_keep_the_history_still_passes_the_host_through_and_lists_it() {
    use puddle_services::learned::LearnedService;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("puddle.db");
    let store = owned(Arc::new(
        Store::open(&path, Arc::new(ManualClock::new(1_000)), Limits::default()).unwrap(),
    ));
    let (injection, terminations, _, events) = injection_with_events(&store);
    let service = crate::learned::HostLearned::new(Arc::clone(&terminations), Arc::clone(&store));
    let ws = workspace("alpha");
    injection.begin(&ws, false).await.unwrap();
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch("DROP TABLE learned_hosts")
        .unwrap();

    let learned = Arc::clone(
        terminations
            .termination(&ws)
            .unwrap()
            .interception()
            .learned(),
    );
    learned.learn(&Host::parse_normalised("pinned.example").unwrap());
    assert!(
        learned.contains(&Host::parse_normalised("pinned.example").unwrap()),
        "the host is passed through whether or not its history was written"
    );
    assert_eq!(
        told(&events),
        [Event::PassThroughLearned {
            workspace: ws.clone(),
            host: "pinned.example".to_owned(),
            count: 1,
        }],
        "the user is told, with a count of one"
    );
    let listed = service.list(&handle(&ws));
    assert_eq!(
        listed
            .iter()
            .map(|h| (h.host.as_str(), h.passed_through, h.count))
            .collect::<Vec<_>>(),
        [("pinned.example", true, 1)],
        "a host passed through is listed even without its history"
    );
    assert!(
        !service.discard(&handle(&ws), "not a host"),
        "not a name: nothing to discard"
    );
    assert!(
        service.discard(&handle(&ws), "pinned.example"),
        "it was passed through, so it is discarded although its history cannot be dropped"
    );
    assert!(!learned.contains(&Host::parse_normalised("pinned.example").unwrap()));
}
#[tokio::test]
async fn the_history_of_a_refused_host_outlives_the_workspace_run_but_the_pass_through_does_not() {
    use puddle_services::learned::LearnedService;

    let store = store();
    let (injection, terminations) = injection(&store);
    let service = crate::learned::HostLearned::new(Arc::clone(&terminations), Arc::clone(&store));
    let ws = workspace("alpha");
    injection.begin(&ws, false).await.unwrap();
    terminations
        .termination(&ws)
        .unwrap()
        .interception()
        .learned()
        .learn(&Host::parse_normalised("pinned.example").unwrap());
    assert!(service.list(&handle(&ws))[0].passed_through);
    injection.end(&ws);
    let kept = service.list(&handle(&ws));
    assert_eq!(kept.len(), 1);
    assert_eq!(
        (
            kept[0].host.as_str(),
            kept[0].passed_through,
            kept[0].expires_in_secs,
            kept[0].count
        ),
        ("pinned.example", false, None, 1),
        "a stopped workspace passes nothing through but remembers"
    );
    injection.begin(&ws, false).await.unwrap();
    assert!(
        !service.list(&handle(&ws))[0].passed_through,
        "a start begins with none passed through"
    );
}
#[tokio::test]
async fn the_learned_limits_reach_a_running_workspace_without_forgetting_what_it_learned() {
    let store = store();
    let (injection, terminations) = injection(&store);
    let ws = workspace("alpha");
    let host = |h: &str| Host::parse_normalised(h).unwrap();
    injection.begin(&ws, false).await.unwrap();
    let learned = Arc::clone(
        terminations
            .termination(&ws)
            .unwrap()
            .interception()
            .learned(),
    );
    assert_eq!(
        (learned.ttl(), learned.cap()),
        (Duration::from_hours(12), 1024),
        "the built-in limits"
    );
    for n in 0..3 {
        learned.learn(&host(&format!("h{n}.example")));
    }
    store
        .set_learned_limits(puddle_store::LearnedLimits {
            ttl_secs: 600,
            cap: 2,
        })
        .unwrap();
    injection.resync(&ws).unwrap();
    assert_eq!(
        (learned.ttl(), learned.cap()),
        (Duration::from_secs(600), 2)
    );
    assert_eq!(learned.list().len(), 2, "the oldest made room");
    assert!(Arc::ptr_eq(
        terminations
            .termination(&ws)
            .unwrap()
            .interception()
            .learned(),
        &learned
    ));
}
#[test]
fn the_stored_defaults_match_the_proxy_built_in_values() {
    let built_in = puddle_store::LearnedLimits::default();
    assert_eq!(
        Duration::from_secs(u64::from(built_in.ttl_secs)),
        puddle_proxy::LEARNED_TTL
    );
    assert_eq!(built_in.cap as usize, puddle_proxy::LEARNED_CAP);
}
