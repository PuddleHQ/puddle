// SPDX-License-Identifier: GPL-3.0-or-later
//! Operating-system notifications: which events raise one, what it says, and how a burst of
//! requests from one workspace is folded into one. Every choice follows a Settings switch
//! ([`UiPrefs`]); nothing here can approve anything, a notification only opens the inbox.
//!
//! [`Notifier`] is pure: it is told the time, the preferences and whether the window is in front,
//! and returns the notices to show. [`show`] hands one to the operating system.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use puddle_settings::UiPrefs;
use puddle_types::{Event, PendingSummary, WorkspaceName, WorkspaceStatus};
use tauri::{AppHandle, Runtime};
use tauri_plugin_notification::NotificationExt;

/// Requests from one workspace inside this window share one notification: the first shows at
/// once, the rest are folded into a single summary when the window ends.
pub const GROUP_WINDOW: Duration = Duration::from_secs(10);

/// How many hosts a summary names before it says "and N more".
const NAMED_IN_SUMMARY: usize = 3;

/// The longest name or host a notice quotes.
const MAX_QUOTED_CHARS: usize = 80;

/// The extra a notice carries so a click knows which request it was about.
pub const REQUEST_EXTRA: &str = "request";

/// One notification to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    /// The first line.
    pub title: String,
    /// The text under it.
    pub body: String,
    /// The request a click opens the inbox at; `None` opens the inbox as it is.
    pub request: Option<i64>,
}

/// A name or host from a workspace, made safe to put in a notification: control characters and
/// the characters notification servers read as markup become `?`, and a long one is cut.
#[must_use]
pub fn clean(text: &str) -> String {
    let mut out: String = text
        .chars()
        .take(MAX_QUOTED_CHARS)
        .map(|c| {
            if c.is_control() || matches!(c, '<' | '>' | '&') {
                '?'
            } else {
                c
            }
        })
        .collect();
    if text.chars().count() > MAX_QUOTED_CHARS {
        out.push('…');
    }
    out
}

/// What a held request is remembered by.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Held {
    id: i64,
    host: String,
    port: u16,
}

#[derive(Debug)]
struct Group {
    /// When the window that started with this group's first notification ends.
    ends: Instant,
    held: Vec<Held>,
}

/// Decides what to show for each event.
#[derive(Debug)]
pub struct Notifier {
    window: Duration,
    groups: HashMap<WorkspaceName, Group>,
}

impl Default for Notifier {
    fn default() -> Self {
        Self::with_window(GROUP_WINDOW)
    }
}

/// Whether a notification may show now, by the switches: the master switch for requests, and
/// "only while Puddle is not in front".
fn allowed(prefs: &UiPrefs, master: bool, in_front: bool) -> bool {
    master && !(prefs.notify_only_unfocused() && in_front)
}

impl Notifier {
    /// A notifier whose windows last `window` instead of [`GROUP_WINDOW`] (tests use a short one).
    #[must_use]
    pub fn with_window(window: Duration) -> Self {
        Self {
            window,
            groups: HashMap::new(),
        }
    }

    /// What to show for `event`, given the preferences and whether Puddle's window is in front.
    pub fn handle(
        &mut self,
        now: Instant,
        prefs: &UiPrefs,
        in_front: bool,
        event: &Event,
    ) -> Vec<Notice> {
        match event {
            Event::PendingOpened { request } => {
                let mut notices = self.flush_ended(now, prefs, in_front);
                if allowed(prefs, prefs.notifications(), in_front) {
                    notices.extend(self.offer(now, request));
                }
                notices
            }
            Event::OomKill {
                workspace, process, ..
            } if allowed(prefs, prefs.notify_oom(), in_front) => vec![Notice {
                title: format!("{} ran out of memory", clean(workspace.as_str())),
                body: format!(
                    "{} was ended. Give the workspace more memory in its settings; it applies \
                     the next time it starts.",
                    clean(process)
                ),
                request: None,
            }],
            Event::StatusChanged {
                workspace,
                status: WorkspaceStatus::Crashed,
            } if allowed(prefs, prefs.notify_unexpected_stop(), in_front) => vec![Notice {
                title: format!("{} stopped", clean(workspace.as_str())),
                body: "It stopped or crashed without you or Puddle asking it to. Start it again \
                       from Puddle."
                    .to_owned(),
                request: None,
            }],
            _ => Vec::new(),
        }
    }

    /// The summaries whose window has ended; call it when [`Notifier::deadline`] passes.
    pub fn flush(&mut self, now: Instant, prefs: &UiPrefs, in_front: bool) -> Vec<Notice> {
        self.flush_ended(now, prefs, in_front)
    }

    /// When the next summary is due, if any request is being held.
    #[must_use]
    pub fn deadline(&self) -> Option<Instant> {
        self.groups
            .values()
            .filter(|g| !g.held.is_empty())
            .map(|g| g.ends)
            .min()
    }

    /// A request opened: shown at once if its workspace has no window running, else held.
    fn offer(&mut self, now: Instant, request: &PendingSummary) -> Option<Notice> {
        let held = Held {
            id: request.id,
            host: request.host.clone(),
            port: request.port,
        };
        if let Some(group) = self.groups.get_mut(&request.workspace)
            && now < group.ends
        {
            group.held.push(held);
            return None;
        }
        self.groups.insert(
            request.workspace.clone(),
            Group {
                ends: now + self.window,
                held: Vec::new(),
            },
        );
        Some(Notice {
            title: format!("{} is asking for access", clean(request.workspace.as_str())),
            body: format!(
                "{}:{} is waiting for your decision.",
                clean(&held.host),
                held.port
            ),
            request: Some(held.id),
        })
    }

    /// Ends every window that is over: held requests become one summary per workspace (shown
    /// only if the switches still allow it) and the workspace's next window starts after it.
    fn flush_ended(&mut self, now: Instant, prefs: &UiPrefs, in_front: bool) -> Vec<Notice> {
        let show = allowed(prefs, prefs.notifications(), in_front);
        let mut notices = Vec::new();
        let mut ended: Vec<(WorkspaceName, Group)> =
            self.groups.extract_if(|_, g| g.ends <= now).collect();
        ended.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, group) in ended {
            if group.held.is_empty() {
                continue;
            }
            if show {
                notices.push(summary(&name, &group.held));
                self.groups.insert(
                    name,
                    Group {
                        ends: now + self.window,
                        held: Vec::new(),
                    },
                );
            }
        }
        notices
    }
}

/// One notice for the requests a workspace made while its window was running.
fn summary(workspace: &WorkspaceName, held: &[Held]) -> Notice {
    let named: Vec<String> = held
        .iter()
        .take(NAMED_IN_SUMMARY)
        .map(|h| format!("{}:{}", clean(&h.host), h.port))
        .collect();
    let more = held.len().saturating_sub(NAMED_IN_SUMMARY);
    let list = if more == 0 {
        named.join(", ")
    } else {
        format!("{} and {more} more", named.join(", "))
    };
    Notice {
        title: format!(
            "{} is asking for {} more",
            clean(workspace.as_str()),
            held.len()
        ),
        body: format!(
            "{list} {} waiting for your decision.",
            if held.len() == 1 { "is" } else { "are" }
        ),
        request: held.first().map(|h| h.id),
    }
}

/// The sound a notification plays when the sound switch is on: the platform's own name for its
/// ordinary new-message sound. Windows names are the toast sounds, Linux ones are freedesktop
/// sound names, macOS ones are the files in the system's sounds folder.
#[cfg(windows)]
const SOUND: &str = "Default";
#[cfg(target_os = "macos")]
const SOUND: &str = "Ping";
#[cfg(not(any(windows, target_os = "macos")))]
const SOUND: &str = "message-new-instant";

/// Shows `notice` through the operating system, with the system sound when `sound` is on. No
/// sound is asked for otherwise: the platforms play none then. A notice the system refuses is
/// logged: the same event is also in the inbox and the in-app notices.
pub fn show<R: Runtime>(app: &AppHandle<R>, notice: &Notice, sound: bool) {
    let mut builder = app
        .notification()
        .builder()
        .title(&notice.title)
        .body(&notice.body);
    if let Some(id) = notice.request {
        builder = builder.extra(REQUEST_EXTRA, id);
    }
    if sound {
        builder = builder.sound(SOUND);
    }
    if let Err(err) = builder.show() {
        tracing::warn!(%err, "could not show a notification");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: [Notice; 0] = [];

    fn name(s: &str) -> WorkspaceName {
        WorkspaceName::new(s).unwrap()
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
}
