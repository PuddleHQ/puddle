// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

const ANN: UserId = UserId::OWNER;
const NOW: u64 = 10_000_000;

fn report(
    client: &str,
    kind: ClientKind,
    visible: bool,
    idle_ms: u64,
    notify: bool,
) -> PresenceReport {
    PresenceReport {
        client: client.into(),
        kind,
        visible,
        idle_ms,
        notify,
    }
}

fn tab(client: &str, idle_ms: u64) -> PresenceReport {
    report(client, ClientKind::Browser, true, idle_ms, true)
}

fn picked(clients: &[&str], desktop: bool) -> Picked {
    Picked {
        clients: clients.iter().map(ToString::to_string).collect(),
        desktop,
    }
}

#[test]
fn with_nobody_reporting_the_desktop_app_is_told_as_before() {
    assert_eq!(Presence::default().pick(ANN, NOW), picked(&[], true));
}

#[test]
fn the_client_used_in_the_last_two_minutes_is_the_only_one_told() {
    let p = Presence::default();
    p.report(ANN, &tab("desk", 119_000), NOW);
    p.report(ANN, &tab("phone", 30_000), NOW);
    p.report(ANN, &tab("old", 130_000), NOW);
    assert_eq!(p.pick(ANN, NOW), picked(&["phone"], false));
}

#[test]
fn a_tie_goes_to_the_one_in_front_and_then_to_the_lower_id() {
    let p = Presence::default();
    p.report(
        ANN,
        &report("a", ClientKind::Browser, false, 5_000, true),
        NOW,
    );
    p.report(
        ANN,
        &report("b", ClientKind::Browser, true, 5_000, true),
        NOW,
    );
    p.report(
        ANN,
        &report("c", ClientKind::Browser, true, 5_000, true),
        NOW,
    );
    assert_eq!(p.pick(ANN, NOW), picked(&["b"], false));
}

#[test]
fn without_recent_use_every_client_that_wants_notifications_is_told() {
    let p = Presence::default();
    p.report(ANN, &tab("b", 300_000), NOW);
    p.report(ANN, &tab("a", 400_000), NOW);
    p.report(
        ANN,
        &report("off", ClientKind::Browser, true, 500_000, false),
        NOW,
    );
    p.report(
        ANN,
        &report("app", ClientKind::Desktop, true, 500_000, true),
        NOW,
    );
    assert_eq!(p.pick(ANN, NOW), picked(&["a", "b"], true));
}

#[test]
fn a_client_with_its_switch_off_is_never_told_even_when_it_is_in_use() {
    let p = Presence::default();
    p.report(
        ANN,
        &report("desk", ClientKind::Browser, true, 1_000, false),
        NOW,
    );
    p.report(ANN, &tab("phone", 200_000), NOW);
    assert_eq!(p.pick(ANN, NOW), picked(&["phone"], false));
    let off = Presence::default();
    off.report(
        ANN,
        &report("desk", ClientKind::Browser, true, 1_000, false),
        NOW,
    );
    assert_eq!(off.pick(ANN, NOW), picked(&[], false));
}

#[test]
fn the_desktop_app_in_use_is_told_and_no_tab_is() {
    let p = Presence::default();
    p.report(
        ANN,
        &report("app", ClientKind::Desktop, true, 2_000, true),
        NOW,
    );
    p.report(ANN, &tab("tab", 60_000), NOW);
    assert_eq!(p.pick(ANN, NOW), picked(&[], true));
}

#[test]
fn a_client_that_stopped_reporting_is_not_told() {
    let p = Presence::default();
    p.report(ANN, &tab("gone", 0), NOW);
    p.report(ANN, &tab("here", 60_000), NOW + 60_000);
    // `gone` last reported 76 s ago: past two reports and the slack.
    assert_eq!(p.pick(ANN, NOW + LIVE_MS + 1_000), picked(&["here"], false));
    // Nobody is live any more: back to the desktop app alone.
    assert_eq!(p.pick(ANN, NOW + 10 * LIVE_MS), picked(&[], true));
}

#[test]
fn one_user_never_picks_another_users_clients() {
    let p = Presence::default();
    let bob = UserId::from_stored(2);
    p.report(bob, &tab("bobs", 1_000), NOW);
    assert_eq!(p.pick(ANN, NOW), picked(&[], true));
    assert_eq!(p.pick(bob, NOW), picked(&["bobs"], false));
}

#[test]
fn a_user_keeps_at_most_64_clients_and_the_silent_one_makes_room() {
    let p = Presence::default();
    for n in 0..MAX_CLIENTS {
        p.report(ANN, &tab(&format!("c{n}"), 0), NOW + n as u64);
    }
    p.report(ANN, &tab("new", 0), NOW + 1_000);
    let users = p.users.lock().unwrap();
    let clients = &users[&ANN];
    assert_eq!(clients.len(), MAX_CLIENTS);
    assert!(clients.contains_key("new"));
    assert!(!clients.contains_key("c0"));
}

#[test]
fn a_report_from_a_known_client_replaces_it_and_old_ones_are_forgotten() {
    let p = Presence::default();
    p.report(ANN, &tab("a", 0), NOW);
    p.report(ANN, &tab("stale", 0), NOW);
    p.report(
        ANN,
        &report("a", ClientKind::Browser, false, 0, true),
        NOW + FORGET_MS + 1,
    );
    let users = p.users.lock().unwrap();
    assert_eq!(users[&ANN].len(), 1);
    assert!(!users[&ANN]["a"].visible);
}

#[test]
fn client_ids_are_short_and_plain() {
    assert!(valid_client_id("tab-1_A"));
    assert!(valid_client_id(&"x".repeat(64)));
    assert!(!valid_client_id(""));
    assert!(!valid_client_id(&"x".repeat(65)));
    assert!(!valid_client_id("a b"));
    assert!(!valid_client_id("a\u{202e}b"));
}

fn picker_parts() -> (Arc<Store>, Arc<dyn Clock>) {
    let clock = Arc::new(puddle_store::ManualClock::new(1));
    let store = Store::open_in_memory(clock.clone(), puddle_store::Limits::default()).unwrap();
    (Arc::new(store), clock)
}

#[tokio::test]
async fn the_picker_ends_when_the_hub_is_gone() {
    let hub = Arc::new(EventHub::default());
    let (store, clock) = picker_parts();
    let (_stop, shutdown) = watch::channel(false);
    let picker = spawn_picker(&hub, store, Arc::default(), clock, shutdown);
    drop(hub);
    tokio::time::timeout(std::time::Duration::from_secs(10), picker)
        .await
        .expect("the picker ended")
        .unwrap();
}

#[tokio::test]
async fn the_picker_ends_on_shutdown_and_does_not_count_as_a_stream() {
    let hub = Arc::new(EventHub::default());
    let (store, clock) = picker_parts();
    let (stop, shutdown) = watch::channel(false);
    let picker = spawn_picker(&hub, store, Arc::default(), clock, shutdown);
    assert_eq!(hub.subscribers(), 0);
    stop.send(true).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), picker)
        .await
        .expect("the picker ended")
        .unwrap();
    assert_eq!(hub.subscribers(), 0);
}
