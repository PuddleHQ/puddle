// SPDX-License-Identifier: AGPL-3.0-or-later
//! Operating-system notifications: which events raise one, what it says, and how a burst of
//! requests from one workspace is folded into one. Every choice follows a Settings switch
//! ([`UiPrefs`]); nothing here can approve anything, a notification only opens the inbox.
//!
//! [`Notifier`] is pure: it is told the time, the preferences and whether the window is in front,
//! and returns the notices to show. [`show`] hands one to the operating system.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use puddle_settings::UiPrefs;
use puddle_types::{DiskKind, DiskSpaceLevel, Event, PendingSummary, WorkspaceId, WorkspaceStatus};
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

/// The workspace an event's notice would name, if it has one.
#[must_use]
pub fn workspace_of(event: &Event) -> Option<&WorkspaceId> {
    match event {
        Event::PendingOpened { request } => Some(&request.workspace),
        Event::OomKill { workspace, .. }
        | Event::DiskNearlyFull { workspace, .. }
        | Event::StatusChanged { workspace, .. } => Some(workspace),
        _ => None,
    }
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
    groups: HashMap<WorkspaceId, Group>,
    /// What the user calls each workspace, as last told ([`Notifier::name`]); a workspace not
    /// in it is shown by its id.
    labels: HashMap<WorkspaceId, String>,
    /// The free-space level the last event named, so tighter space notifies and recovery does not.
    disk: DiskSpaceLevel,
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
            labels: HashMap::new(),
            disk: DiskSpaceLevel::Ok,
        }
    }

    /// Tells the notifier what the user calls `workspace`; its notices show that, not the id.
    pub fn name(&mut self, workspace: &WorkspaceId, label: &str) {
        if self.labels.get(workspace).map(String::as_str) != Some(label) {
            self.labels.insert(workspace.clone(), label.to_owned());
        }
    }

    /// What to call `workspace` in a notice, made safe to show.
    fn shown(&self, workspace: &WorkspaceId) -> String {
        clean(
            self.labels
                .get(workspace)
                .map_or(workspace.as_str(), String::as_str),
        )
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
                title: format!("{} ran out of memory", self.shown(workspace)),
                body: format!(
                    "{} was ended. Give the workspace more memory in its settings; it applies \
                     the next time it starts.",
                    clean(process)
                ),
                request: None,
            }],
            Event::DiskNearlyFull {
                workspace,
                disk,
                percent,
            } if allowed(prefs, prefs.notify_disk_full(), in_front) => vec![Notice {
                title: format!(
                    "{} {} is {percent} % full",
                    self.shown(workspace),
                    match disk {
                        DiskKind::Root => "root disk",
                        DiskKind::Volume => "volume",
                    }
                ),
                body: match disk {
                    DiskKind::Root => "Free space in the workspace, or reset its root disk from \
                                       the workspace page (packages reinstall on the next \
                                       start, the volume is untouched)."
                        .to_owned(),
                    DiskKind::Volume => "Delete what you no longer need in the workspace; the \
                                         volume cannot grow."
                        .to_owned(),
                },
                request: None,
            }],
            Event::StatusChanged {
                workspace,
                status: WorkspaceStatus::Crashed,
            } if allowed(prefs, prefs.notify_unexpected_stop(), in_front) => vec![Notice {
                title: format!("{} stopped", self.shown(workspace)),
                body: "It stopped or crashed without you or Puddle asking it to. Start it again \
                       from Puddle."
                    .to_owned(),
                request: None,
            }],
            Event::DiskSpaceChanged { level, free_mib } => {
                // Told apart from `allowed`: the level is remembered even while the switch is off,
                // so turning it on later does not announce a state that was there all along.
                let tighter = *level > std::mem::replace(&mut self.disk, *level);
                if tighter && allowed(prefs, prefs.notify_low_disk_space(), in_front) {
                    vec![low_disk_space(*level, *free_mib)]
                } else {
                    Vec::new()
                }
            }
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
            title: format!("{} is asking for access", self.shown(&request.workspace)),
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
        let mut ended: Vec<(WorkspaceId, Group)> =
            self.groups.extract_if(|_, g| g.ends <= now).collect();
        ended.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, group) in ended {
            if group.held.is_empty() {
                continue;
            }
            if show {
                notices.push(summary(&self.shown(&name), &group.held));
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

/// The notice for free space that fell under the warning line or the failure line. `free_mib` is
/// the amount left on the drive that holds Puddle's data.
fn low_disk_space(level: DiskSpaceLevel, free_mib: u64) -> Notice {
    let tenths = free_mib * 10 / 1024;
    let free = if tenths.is_multiple_of(10) {
        format!("{}", tenths / 10)
    } else {
        format!("{}.{}", tenths / 10, tenths % 10)
    };
    let (title, advice) = match level {
        DiskSpaceLevel::Critical => (
            "Disk space is almost gone",
            "Free space now: a build or a tool in a workspace can stop halfway when the drive is full.",
        ),
        _ => (
            "Disk space is running low",
            "Delete workspaces you no longer use and files you do not need before it runs out.",
        ),
    };
    Notice {
        title: title.to_owned(),
        body: format!("{free} GB are left on the drive that holds Puddle's data. {advice}"),
        request: None,
    }
}

/// One notice for the requests a workspace made while its window was running.
fn summary(workspace: &str, held: &[Held]) -> Notice {
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
        title: format!("{} is asking for {} more", workspace, held.len()),
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
mod tests;
