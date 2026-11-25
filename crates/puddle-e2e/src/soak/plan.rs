// SPDX-License-Identifier: AGPL-3.0-or-later
//! The soak run's timeline: the playbook's day (`docs/qa/soak-8h.md`, "Schedule of events" and
//! "Workloads") in playbook minutes, and the profiles that fit it into the time a runner has.
//! A profile only says how many real seconds a playbook hour lasts; the order of the day, the
//! loops' rounds and the sample count per hour stay the playbook's.

use std::time::Duration;

/// Minutes of one playbook hour.
const MINUTES: u64 = 60;

/// How long a run's playbook hour lasts, and how long its night is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Profile {
    /// The name `--profile` takes.
    pub name: &'static str,
    /// Real seconds of one playbook hour.
    pub hour_secs: u64,
    /// Playbook hours of load: the loops and the events.
    pub active_hours: u64,
    /// Playbook hours of the idle night that follows.
    pub idle_hours: u64,
}

/// The real day on a runner with no time limit: 8 hours of load and a 4 hour night (12 h).
pub const FULL: Profile = Profile {
    name: "full",
    hour_secs: 3600,
    active_hours: 8,
    idle_hours: 4,
};

/// The same day in 22.5 minutes an hour (4.5 h), so it fits a hosted runner's 6 hour job limit
/// with the build and the morning round.
pub const HOSTED: Profile = Profile {
    name: "hosted",
    hour_secs: 1350,
    active_hours: 8,
    idle_hours: 4,
};

/// A minute an hour (10 minutes): the pull-request run and the planted-leak canary.
pub const SMOKE: Profile = Profile {
    name: "smoke",
    hour_secs: 60,
    active_hours: 8,
    idle_hours: 2,
};

/// Every profile.
pub const PROFILES: [Profile; 3] = [FULL, HOSTED, SMOKE];

impl Profile {
    /// The profile called `name`.
    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        PROFILES.into_iter().find(|p| p.name == name)
    }

    /// Real time from hour 0 to playbook minute `minute`.
    #[must_use]
    pub const fn at(self, minute: u64) -> Duration {
        Duration::from_millis(minute * self.hour_secs * 1000 / MINUTES)
    }

    /// Real length of the loaded part.
    #[must_use]
    pub const fn active(self) -> Duration {
        self.at(self.active_hours * MINUTES)
    }

    /// Real length of the idle night.
    #[must_use]
    pub const fn idle(self) -> Duration {
        self.at(self.idle_hours * MINUTES)
    }

    /// The sampler's period: 5 playbook minutes, never under a second.
    #[must_use]
    pub fn sample_every(self) -> Duration {
        self.at(5).max(Duration::from_secs(1))
    }

    /// How long the sampler runs alone before the workloads start (M8): 15 playbook minutes.
    #[must_use]
    pub const fn sampler_alone(self) -> Duration {
        self.at(15)
    }
}

/// One event of the schedule, on top of the loops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// 0:00: workloads up, the baseline probes and build times.
    Baseline,
    /// Once an hour: the Inbox answered in a batch (allow what the loops need, deny the rest).
    InboxBatch,
    /// 1:00: W2's workspace stopped and started.
    RestartW2,
    /// 2:00: Puddle quit and started again.
    RestartPuddle,
    /// 2:30: Activity loaded, scrolled to the end, exported (Playwright).
    Activity,
    /// 3:00: 200 rules added and deleted through the API, 20 through the UI (Playwright).
    Rules,
    /// 3:30: 500 new hosts from W3 at once, then the Inbox (Playwright).
    Flood,
    /// 4:30: `puddle backup` while the loops run.
    Backup,
    /// 5:00: the hard kill of `puddle serve`; the next start reconciles.
    HardKill,
    /// 7:00: the Inbox batch with keyboard shortcuts (Playwright).
    InboxKeys,
    /// 8:00: the loops stop; the night begins.
    StopLoops,
}

impl Event {
    /// Whether the event drives the UI (the `--ui` command runs it).
    #[must_use]
    pub const fn in_the_ui(self) -> bool {
        matches!(
            self,
            Self::Activity | Self::Rules | Self::Flood | Self::InboxKeys
        )
    }

    /// The name the run log and the UI command use.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Baseline => "baseline",
            Self::InboxBatch => "inbox-batch",
            Self::RestartW2 => "restart-w2",
            Self::RestartPuddle => "restart-puddle",
            Self::Activity => "activity",
            Self::Rules => "rules",
            Self::Flood => "flood",
            Self::Backup => "backup",
            Self::HardKill => "hard-kill",
            Self::InboxKeys => "inbox-keys",
            Self::StopLoops => "stop-loops",
        }
    }
}

/// The events at fixed hours, as playbook minute and event. The 1:30, 4:00 and 6:00 events are
/// settled elsewhere (docs/qa/soak-8h.md "Who runs what"), so they are not here.
const FIXED: [(u64, Event); 10] = [
    (0, Event::Baseline),
    (60, Event::RestartW2),
    (120, Event::RestartPuddle),
    (150, Event::Activity),
    (180, Event::Rules),
    (210, Event::Flood),
    (270, Event::Backup),
    (300, Event::HardKill),
    (420, Event::InboxKeys),
    (480, Event::StopLoops),
];

/// The day's events in time order: the fixed ones and an Inbox batch at every half past (the
/// playbook answers the Inbox "once an hour", the first at 0:30) except 7:30, where the 7:00
/// keyboard batch already did it. With `workloads` off only the baseline and the stop remain.
#[must_use]
pub fn events(profile: Profile, workloads: bool) -> Vec<(u64, Event)> {
    let end = profile.active_hours * MINUTES;
    let mut out: Vec<(u64, Event)> = FIXED
        .iter()
        .copied()
        .filter(|&(m, e)| {
            m <= end && (workloads || matches!(e, Event::Baseline | Event::StopLoops))
        })
        .map(|(m, e)| (m.min(end), e))
        .collect();
    if workloads {
        out.extend(
            (0..profile.active_hours)
                .map(|h| h * MINUTES + 30)
                .filter(|&m| m != 450)
                .map(|m| (m, Event::InboxBatch)),
        );
    }
    out.sort_by_key(|&(m, _)| m);
    out
}

/// One workload loop of the day.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Workload {
    /// The Rust build loop.
    W1,
    /// The pnpm install and build loop.
    W2,
    /// The simulated agent's rounds against the lab forge.
    W3,
}

impl Workload {
    /// Every loop.
    pub const ALL: [Self; 3] = [Self::W1, Self::W2, Self::W3];

    /// Playbook minutes between rounds.
    #[must_use]
    pub const fn period(self) -> u64 {
        match self {
            Self::W1 => 20,
            Self::W2 => 30,
            Self::W3 => 15,
        }
    }

    /// The workspace name of the playbook.
    #[must_use]
    pub const fn workspace(self) -> &'static str {
        match self {
            Self::W1 => "qa-soak-build",
            Self::W2 => "qa-soak-js",
            Self::W3 => "qa-soak-agent",
        }
    }

    /// The script under the scripts folder that runs one round (`$1` is the round number).
    #[must_use]
    pub const fn script(self) -> &'static str {
        match self {
            Self::W1 => "w1-round.sh",
            Self::W2 => "w2-round.sh",
            Self::W3 => "w3-round.sh",
        }
    }
}

/// The playbook minutes `workload` starts a round at: from 0 every period, while the loops run.
#[must_use]
pub fn rounds(profile: Profile, workload: Workload) -> Vec<u64> {
    (0..profile.active_hours * MINUTES)
        .step_by(usize::try_from(workload.period()).unwrap_or(usize::MAX))
        .collect()
}

#[cfg(test)]
mod tests;
