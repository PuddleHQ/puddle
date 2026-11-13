// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

const NONE: [Notice; 0] = [];

fn name(s: &str) -> WorkspaceId {
    WorkspaceId::new(s).unwrap()
}

fn open(id: i64, workspace: &str, host: &str) -> Event {
    Event::PendingOpened {
        request: PendingSummary {
            id,
            workspace: name(workspace),
            host: host.to_owned(),
            registrable_domain: host.to_owned(),
            port: 443,
            first_seen: 1,
            last_seen: 1,
            attempts: 1,
        },
    }
}

fn secs(s: u64) -> Duration {
    Duration::from_secs(s)
}

/// Defaults except for the switch that makes the window never count as in front.
fn prefs() -> UiPrefs {
    UiPrefs::default()
}

#[test]
fn the_first_request_shows_at_once_and_names_the_workspace_host_and_port() {
    let (mut n, t0) = (Notifier::default(), Instant::now());
    let notices = n.handle(
        t0,
        &prefs(),
        false,
        &open(5, "demo", "registry.example.org"),
    );
    assert_eq!(
        notices,
        [Notice {
            title: "demo is asking for access".to_owned(),
            body: "registry.example.org:443 is waiting for your decision.".to_owned(),
            request: Some(5),
        }]
    );
}

#[test]
fn requests_in_the_window_fold_into_one_summary_when_it_ends() {
    let (mut n, t0) = (Notifier::default(), Instant::now());
    n.handle(t0, &prefs(), false, &open(1, "demo", "a.example"));
    for (i, host) in ["b.example", "c.example", "d.example", "e.example"]
        .iter()
        .enumerate()
    {
        let id = 2 + i64::try_from(i).unwrap();
        let shown = n.handle(t0 + secs(2), &prefs(), false, &open(id, "demo", host));
        assert!(shown.is_empty(), "held, not shown: {shown:?}");
    }
    assert_eq!(n.deadline(), Some(t0 + GROUP_WINDOW));
    assert_eq!(n.flush(t0 + secs(9), &prefs(), false), NONE);
    let summary = n.flush(t0 + GROUP_WINDOW, &prefs(), false);
    assert_eq!(
        summary,
        [Notice {
            title: "demo is asking for 4 more".to_owned(),
            body: "b.example:443, c.example:443, d.example:443 and 1 more are waiting for \
                       your decision."
                .to_owned(),
            request: Some(2),
        }]
    );
    assert_eq!(n.deadline(), None, "the summary holds nothing back");
}

#[test]
fn a_summary_starts_a_new_window_so_a_steady_stream_is_one_notice_per_window() {
    let (mut n, t0) = (Notifier::default(), Instant::now());
    n.handle(t0, &prefs(), false, &open(1, "demo", "a.example"));
    n.handle(t0 + secs(1), &prefs(), false, &open(2, "demo", "b.example"));
    assert_eq!(n.flush(t0 + GROUP_WINDOW, &prefs(), false).len(), 1);
    let during = n.handle(
        t0 + GROUP_WINDOW + secs(1),
        &prefs(),
        false,
        &open(3, "demo", "c.example"),
    );
    assert_eq!(during, NONE);
    assert_eq!(n.flush(t0 + GROUP_WINDOW * 2, &prefs(), false).len(), 1);
}

#[test]
fn a_request_after_a_missed_deadline_brings_the_held_summary_and_starts_a_new_window() {
    let (mut n, t0) = (Notifier::default(), Instant::now());
    n.handle(t0, &prefs(), false, &open(1, "demo", "a.example"));
    n.handle(t0 + secs(1), &prefs(), false, &open(2, "demo", "b.example"));
    let late = n.handle(
        t0 + secs(30),
        &prefs(),
        false,
        &open(3, "demo", "c.example"),
    );
    assert_eq!(late.len(), 1, "{late:?}");
    assert_eq!(late[0].title, "demo is asking for 1 more");
    assert_eq!(late[0].body, "b.example:443 is waiting for your decision.");
    // The summary was this window's notification, so the late request waits for its end.
    assert_eq!(n.deadline(), Some(t0 + secs(30) + GROUP_WINDOW));
}

#[test]
fn a_window_that_ended_with_nothing_held_is_forgotten() {
    let (mut n, t0) = (Notifier::default(), Instant::now());
    n.handle(t0, &prefs(), false, &open(1, "demo", "a.example"));
    assert_eq!(n.flush(t0 + GROUP_WINDOW, &prefs(), false), NONE);
    // The next request is the first of a new window and shows at once.
    let next = n.handle(
        t0 + GROUP_WINDOW * 2,
        &prefs(),
        false,
        &open(2, "demo", "b.example"),
    );
    assert_eq!(next.len(), 1);
}

#[test]
fn each_workspace_has_its_own_window() {
    let (mut n, t0) = (Notifier::default(), Instant::now());
    assert_eq!(
        n.handle(t0, &prefs(), false, &open(1, "one", "a.example"))
            .len(),
        1
    );
    assert_eq!(
        n.handle(t0 + secs(1), &prefs(), false, &open(2, "two", "b.example"))
            .len(),
        1
    );
    assert_eq!(
        n.handle(t0 + secs(2), &prefs(), false, &open(3, "one", "c.example")),
        NONE
    );
}

#[test]
fn the_request_switch_and_the_in_front_switch_decide() {
    let t0 = Instant::now();
    let ask = open(1, "demo", "a.example");
    let mut off = prefs();
    off.notifications = Some(false);
    assert_eq!(Notifier::default().handle(t0, &off, false, &ask), NONE);

    // In front: nothing by default, a notice when the user turned "only when not in front" off.
    assert_eq!(Notifier::default().handle(t0, &prefs(), true, &ask), NONE);
    let mut always = prefs();
    always.notify_only_unfocused = Some(false);
    assert_eq!(Notifier::default().handle(t0, &always, true, &ask).len(), 1);
}

#[test]
fn a_summary_is_dropped_when_the_window_came_to_the_front_meanwhile() {
    let (mut n, t0) = (Notifier::default(), Instant::now());
    n.handle(t0, &prefs(), false, &open(1, "demo", "a.example"));
    n.handle(t0 + secs(1), &prefs(), false, &open(2, "demo", "b.example"));
    assert_eq!(n.flush(t0 + GROUP_WINDOW, &prefs(), true), NONE);
    assert_eq!(n.deadline(), None);
}

#[test]
fn a_request_held_while_not_in_front_is_not_held_while_in_front() {
    let (mut n, t0) = (Notifier::default(), Instant::now());
    n.handle(t0, &prefs(), false, &open(1, "demo", "a.example"));
    assert_eq!(
        n.handle(t0 + secs(1), &prefs(), true, &open(2, "demo", "b.example")),
        NONE
    );
    assert_eq!(n.deadline(), None, "nothing was held");
}

#[test]
fn every_notice_shows_the_display_name_never_the_id() {
    let (mut n, t0) = (Notifier::default(), Instant::now());
    n.name(&name("a1b2c3"), "Shop API ü");
    let kill = Event::oom_kill(name("a1b2c3"), 7, "node");
    let crashed = Event::StatusChanged {
        workspace: name("a1b2c3"),
        status: WorkspaceStatus::Crashed,
    };
    let full = Event::DiskNearlyFull {
        workspace: name("a1b2c3"),
        disk: DiskKind::Volume,
        percent: 91,
    };
    let titles = |events: [Event; 3], n: &mut Notifier| -> Vec<String> {
        events
            .iter()
            .flat_map(|e| n.handle(t0, &prefs(), false, e))
            .map(|notice| notice.title)
            .collect()
    };
    assert_eq!(
        titles([kill, crashed, full], &mut n),
        [
            "Shop API ü ran out of memory",
            "Shop API ü stopped",
            "Shop API ü volume is 91 % full"
        ]
    );
    let first = n.handle(t0, &prefs(), false, &open(1, "a1b2c3", "a.example"));
    assert_eq!(first[0].title, "Shop API ü is asking for access");
    n.handle(
        t0 + secs(1),
        &prefs(),
        false,
        &open(2, "a1b2c3", "b.example"),
    );
    let summary = n.flush(t0 + GROUP_WINDOW, &prefs(), false);
    assert_eq!(summary[0].title, "Shop API ü is asking for 1 more");
    let before_naming = Notifier::default().handle(
        t0,
        &prefs(),
        false,
        &Event::oom_kill(name("a1b2c3"), 7, "node"),
    );
    assert_eq!(before_naming[0].title, "a1b2c3 ran out of memory");
}

#[test]
fn an_out_of_memory_kill_notifies_by_its_own_switch() {
    let t0 = Instant::now();
    let kill = Event::oom_kill(name("demo"), 7, "node");
    let notices = Notifier::default().handle(t0, &prefs(), false, &kill);
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].title, "demo ran out of memory");
    let body = &notices[0].body;
    assert!(body.starts_with("node was ended."), "{body}");
    assert_eq!(notices[0].request, None);
    let mut off = prefs();
    off.notify_oom = Some(false);
    assert_eq!(Notifier::default().handle(t0, &off, false, &kill), NONE);
    assert_eq!(Notifier::default().handle(t0, &prefs(), true, &kill), NONE);
}

#[test]
fn free_space_notifies_when_it_gets_tighter_by_its_own_switch_and_not_when_it_recovers() {
    let t0 = Instant::now();
    let at = |level, free_mib| Event::DiskSpaceChanged { level, free_mib };
    let mut n = Notifier::default();
    let low = n.handle(t0, &prefs(), false, &at(DiskSpaceLevel::Low, 15_360));
    assert_eq!(low.len(), 1);
    assert_eq!(low[0].title, "Disk space is running low");
    assert_eq!(
        low[0].body.split(". ").next(),
        Some("15 GB are left on the drive that holds Puddle's data")
    );
    assert_eq!(low[0].request, None);
    let critical = n.handle(t0, &prefs(), false, &at(DiskSpaceLevel::Critical, 3_584));
    assert_eq!(critical.len(), 1);
    assert_eq!(critical[0].title, "Disk space is almost gone");
    assert_eq!(
        critical[0].body.split(". ").next().map(|s| &s[..15]),
        Some("3.5 GB are left")
    );
    // Partly back (critical to low) and fully back (ok) are good news: no notice.
    assert_eq!(
        n.handle(t0, &prefs(), false, &at(DiskSpaceLevel::Low, 8_192)),
        NONE
    );
    assert_eq!(
        n.handle(t0, &prefs(), false, &at(DiskSpaceLevel::Ok, 30_720)),
        NONE
    );
    // Crossing the same line again is a new crossing.
    assert_eq!(
        n.handle(t0, &prefs(), false, &at(DiskSpaceLevel::Low, 9_000))
            .len(),
        1
    );
}

#[test]
fn the_free_space_switch_and_the_in_front_switch_decide_and_the_level_is_remembered_while_off() {
    let t0 = Instant::now();
    let at = |level| Event::DiskSpaceChanged {
        level,
        free_mib: 4_096,
    };
    let mut off = prefs();
    off.notify_low_disk_space = Some(false);
    let mut n = Notifier::default();
    assert_eq!(n.handle(t0, &off, false, &at(DiskSpaceLevel::Low)), NONE);
    // Its own switch only: the others being off changes nothing.
    let mut others = prefs();
    others.notify_oom = Some(false);
    others.notifications = Some(false);
    assert_eq!(
        Notifier::default()
            .handle(t0, &others, false, &at(DiskSpaceLevel::Low))
            .len(),
        1
    );
    // The state that was there while the switch was off is not announced when it comes on.
    assert_eq!(
        n.handle(t0, &prefs(), false, &at(DiskSpaceLevel::Low)),
        NONE
    );
    assert_eq!(
        Notifier::default().handle(t0, &prefs(), true, &at(DiskSpaceLevel::Low)),
        NONE
    );
    let mut always = prefs();
    always.notify_only_unfocused = Some(false);
    assert_eq!(
        Notifier::default()
            .handle(t0, &always, true, &at(DiskSpaceLevel::Low))
            .len(),
        1
    );
}

#[test]
fn a_full_disk_notifies_by_its_own_switch_and_names_the_disk() {
    let t0 = Instant::now();
    let full = |disk, percent| Event::DiskNearlyFull {
        workspace: name("demo"),
        disk,
        percent,
    };
    let root = Notifier::default().handle(t0, &prefs(), false, &full(DiskKind::Root, 96));
    assert_eq!(root.len(), 1);
    assert_eq!(root[0].title, "demo root disk is 96 % full");
    let body = &root[0].body;
    assert!(body.contains("reset its root disk"), "{body}");
    let volume = Notifier::default().handle(t0, &prefs(), false, &full(DiskKind::Volume, 100));
    assert_eq!(volume[0].title, "demo volume is 100 % full");
    let mut off = prefs();
    off.notify_disk_full = Some(false);
    assert_eq!(
        Notifier::default().handle(t0, &off, false, &full(DiskKind::Root, 96)),
        NONE
    );
    // Its own switch only: the others being off changes nothing.
    let mut others = prefs();
    others.notify_oom = Some(false);
    others.notifications = Some(false);
    assert_eq!(
        Notifier::default()
            .handle(t0, &others, false, &full(DiskKind::Root, 96))
            .len(),
        1
    );
    assert_eq!(
        Notifier::default().handle(t0, &prefs(), true, &full(DiskKind::Root, 96)),
        NONE
    );
}

#[test]
fn only_a_crash_is_an_unexpected_stop_and_it_has_its_own_switch() {
    let t0 = Instant::now();
    let status = |status| Event::StatusChanged {
        workspace: name("demo"),
        status,
    };
    let crashed =
        Notifier::default().handle(t0, &prefs(), false, &status(WorkspaceStatus::Crashed));
    assert_eq!(crashed[0].title, "demo stopped");
    for fine in [
        WorkspaceStatus::Stopped,
        WorkspaceStatus::Draining,
        WorkspaceStatus::Running,
    ] {
        assert!(
            Notifier::default()
                .handle(t0, &prefs(), false, &status(fine))
                .is_empty(),
            "{fine}"
        );
    }
    let mut off = prefs();
    off.notify_unexpected_stop = Some(false);
    assert_eq!(
        Notifier::default().handle(t0, &off, false, &status(WorkspaceStatus::Crashed)),
        NONE
    );
}

#[test]
fn other_events_notify_nobody() {
    assert_eq!(
        Notifier::default().handle(Instant::now(), &prefs(), false, &Event::RulesChanged {}),
        NONE
    );
}

#[test]
fn what_a_workspace_controls_cannot_inject_markup_or_flood_a_notice() {
    assert_eq!(clean("a<b>&c\u{1b}[2J"), "a?b??c?[2J");
    let long = "x".repeat(200);
    let cleaned = clean(&long);
    assert_eq!(cleaned.chars().count(), MAX_QUOTED_CHARS + 1);
    assert!(cleaned.ends_with('…'));
    assert_eq!(clean("fine.example.org"), "fine.example.org");
}
