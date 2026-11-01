// SPDX-License-Identifier: GPL-3.0-or-later
//! Keeps the capture rules in place for the life of the agent.
//!
//! Each tick makes sure the interface exists, works out which interface is outbound, and compares
//! the kernel's table with what was installed last. A table that is missing or changed (a guest
//! running `nft flush ruleset` as root, or the default route moving to another interface) is put
//! back at once, and the host is told when it was the guest. Capture is not enforcement:
//! this is what keeps ordinary tools working, not what keeps a hostile guest in.

use std::net::SocketAddrV4;
use std::time::Duration;

use puddle_agent_proto::AgentMessage;
use tokio::sync::mpsc;

use super::link;
use super::nft::{self, Fingerprint};
use super::rules::{Plan, STAND_IN_NET, Settings};

/// What the guard needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Guard {
    /// The stub DNS address.
    pub stub: SocketAddrV4,
    /// The transparent listener's port.
    pub listen_port: u16,
    /// The dummy interface that carries the stub's address.
    pub interface: String,
    /// How often the table is checked.
    pub poll: Duration,
    /// The netfilter log group refused UDP is reported on; `None` leaves the logging out.
    pub log_group: Option<u16>,
}

/// Why the table was written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    /// First time since the agent started.
    Start,
    /// The outbound interface or the settings changed.
    Changed,
    /// The table had been deleted.
    Missing,
    /// The table was edited.
    Edited,
}

impl Guard {
    /// The rule settings for outbound interface `wan`.
    #[must_use]
    pub fn settings(&self, wan: &str, log_group: Option<u16>) -> Settings {
        Settings {
            stub: *self.stub.ip(),
            stub_port: self.stub.port(),
            stand_ins: STAND_IN_NET,
            listen_port: self.listen_port,
            wan: wan.to_owned(),
            log_group,
        }
    }

    /// The prefix the stub's address gets on the interface: the whole stand-in range when it is in
    /// it (which also routes the range), else just the address.
    fn prefix(&self) -> u8 {
        if super::table::in_range(*self.stub.ip()) {
            STAND_IN_NET.prefix
        } else {
            32
        }
    }
}

/// What the guard remembers between passes.
#[derive(Debug, Default)]
struct State {
    /// What was installed last, and what the kernel then held.
    installed: Option<(Plan, Fingerprint)>,
    /// The kernel refused the rules with the UDP logging but took them without it, so they go in
    /// without it from now on.
    without_log: bool,
}

/// One pass: what to do with the kernel's table now, given what was installed.
fn tick(guard: &Guard, state: &mut State) -> std::io::Result<Option<Why>> {
    let wan = link::setup(&guard.interface, *guard.stub.ip(), guard.prefix())?;
    let log = guard.log_group.filter(|_| !state.without_log);
    let plan = Plan::capture(&guard.settings(&wan, log));
    let current = nft::read()?;
    let why = match (state.installed.as_ref(), current) {
        (None, _) => Why::Start,
        (Some((last, _)), _) if *last != plan => Why::Changed,
        (Some(_), None) => Why::Missing,
        (Some((_, kept)), Some(now)) if *kept != now => Why::Edited,
        (Some(_), Some(_)) => return Ok(None),
    };
    apply(guard, &wan, log, plan, state, nft::install)?;
    Ok(Some(why))
}

/// Installs `plan` through `install` and remembers what was installed.
fn apply(
    guard: &Guard,
    wan: &str,
    log: Option<u16>,
    plan: Plan,
    state: &mut State,
    install: impl Fn(&Plan) -> std::io::Result<Option<Fingerprint>>,
) -> std::io::Result<()> {
    let (plan, fingerprint) = match install(&plan) {
        Ok(fingerprint) => (plan, fingerprint),
        Err(err) if log.is_some() => {
            // A kernel without the log target refuses the whole batch. Refusing UDP matters more
            // than reporting it, so try again without the logging; if that fails too, the cause
            // was something else and the logging stays.
            let quiet = Plan::capture(&guard.settings(wan, None));
            let fingerprint = install(&quiet).map_err(|_| err)?;
            tracing::warn!(
                "the kernel refused the capture rules with UDP logging; installed without it, so refused UDP is no longer reported to the host"
            );
            state.without_log = true;
            (quiet, fingerprint)
        }
        Err(err) => return Err(err),
    };
    // A table the guest deleted right after the batch was taken has nothing to compare: the empty
    // fingerprint makes the next pass see it as missing (or edited, if the guest put one back).
    let fingerprint = fingerprint.unwrap_or_default();
    state.installed = Some((plan, fingerprint));
    Ok(())
}

/// Runs `tick` on a schedule until the agent stops. Failures are logged, told to the host once
/// per outage, and retried: a kernel that can't do it today may be able to after a module loads.
pub async fn keep(guard: Guard, reports: mpsc::Sender<AgentMessage>) {
    let mut state = State::default();
    let mut failures = 0u32;
    // Whether this outage has been told to the host: a queue that is full at boot (nobody reads
    // yet) leaves it untold, so the next pass tries again.
    let mut told = false;
    // The host was told of an outage and not yet that it ended: a queue that is full at that
    // moment leaves it pending, so the next pass tells it again.
    let mut recovery_owed = false;
    loop {
        let g = guard.clone();
        let result = tokio::task::spawn_blocking(move || {
            let outcome = tick(&g, &mut state);
            (state, outcome)
        })
        .await;
        let outcome = match result {
            Ok((kept, outcome)) => {
                state = kept;
                outcome
            }
            Err(err) => {
                tracing::error!(error = %err, "capture guard task failed");
                return;
            }
        };
        match outcome {
            Ok(None) => {
                failures = 0;
                recovered(&mut told, &mut recovery_owed, &reports);
            }
            Ok(Some(why)) => {
                failures = 0;
                recovered(&mut told, &mut recovery_owed, &reports);
                announce(why, state.installed.as_ref().map(|(plan, _)| plan));
                if let Some(report) = report_for(why) {
                    // A full queue only means the host is not listening; the log has it.
                    let _ = reports.try_send(report);
                }
            }
            Err(err) => {
                // A new outage supersedes a recovery the host never heard.
                recovery_owed = false;
                if !told {
                    // The tools in the workspace see an offline network until this changes.
                    told = reports
                        .try_send(AgentMessage::capture_unavailable(err.to_string()))
                        .is_ok();
                }
                if failures.is_multiple_of(60) {
                    tracing::warn!(error = %err, "capture rules could not be installed; trying again");
                }
                failures = failures.saturating_add(1);
            }
        }
        tokio::time::sleep(guard.poll).await;
    }
}

/// An outage the host heard of is over: tells the host the rules are installed, until it has.
fn recovered(told: &mut bool, owed: &mut bool, reports: &mpsc::Sender<AgentMessage>) {
    if *told {
        *told = false;
        *owed = true;
    }
    if *owed {
        *owed = reports.try_send(AgentMessage::capture_ready()).is_err();
    }
}

fn announce(why: Why, plan: Option<&Plan>) {
    match why {
        Why::Start => tracing::info!(rules = ?plan.map(Plan::shape), "capture rules installed"),
        Why::Changed => tracing::info!("capture rules rewritten for a changed network"),
        Why::Missing => {
            tracing::warn!("the capture table was deleted in the guest; rules restored");
        }
        Why::Edited => tracing::warn!("the capture table was changed in the guest; rules restored"),
    }
}

/// What the host is told, if anything.
#[must_use]
pub fn report_for(why: Why) -> Option<AgentMessage> {
    match why {
        Why::Start | Why::Changed => None,
        Why::Missing => Some(AgentMessage::capture_tampered(true)),
        Why::Edited => Some(AgentMessage::capture_tampered(false)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guard() -> Guard {
        Guard {
            stub: "198.18.0.1:53".parse().unwrap(),
            listen_port: 3129,
            interface: "dummy0".to_owned(),
            poll: Duration::from_millis(500),
            log_group: Some(17),
        }
    }

    #[test]
    fn settings_carry_the_stub_the_port_and_the_outbound_interface() {
        let s = guard().settings("eth0", Some(17));
        assert_eq!(s.stub, std::net::Ipv4Addr::new(198, 18, 0, 1));
        assert_eq!(
            (s.stub_port, s.listen_port, s.wan.as_str()),
            (53, 3129, "eth0")
        );
        assert_eq!(s.log_group, Some(17));
    }

    #[test]
    fn the_stub_inside_the_stand_in_range_routes_the_whole_range() {
        assert_eq!(guard().prefix(), 15);
        let mut other = guard();
        other.stub = "10.9.9.9:53".parse().unwrap();
        assert_eq!(other.prefix(), 32);
    }

    /// The install takes the batch, then the guest deletes the table before it can be read back.
    #[test]
    fn a_table_deleted_right_after_the_install_does_not_switch_the_udp_logging_off() {
        let g = guard();
        let mut state = State::default();
        let plan = Plan::capture(&g.settings("eth0", Some(17)));
        apply(&g, "eth0", Some(17), plan, &mut state, |_| Ok(None)).unwrap();
        assert!(!state.without_log, "the kernel never refused the logging");
        let (kept, fingerprint) = state.installed.expect("the install is remembered");
        assert_eq!(kept, Plan::capture(&g.settings("eth0", Some(17))));
        assert_eq!(
            fingerprint,
            Fingerprint::default(),
            "so the next pass sees a missing table"
        );
    }

    #[test]
    fn only_the_guests_doing_is_reported_to_the_host() {
        assert_eq!(report_for(Why::Start), None);
        assert_eq!(report_for(Why::Changed), None);
        assert_eq!(
            report_for(Why::Missing),
            Some(AgentMessage::capture_tampered(true))
        );
        assert_eq!(
            report_for(Why::Edited),
            Some(AgentMessage::capture_tampered(false))
        );
    }

    #[test]
    fn the_host_hears_the_rules_are_back_once_and_only_after_an_outage_it_heard_of() {
        let (tx, mut rx) = mpsc::channel(1);
        let (mut told, mut owed) = (false, false);
        recovered(&mut told, &mut owed, &tx);
        assert!(rx.try_recv().is_err(), "no outage, nothing to end"); // flake-ok: `recovered` sends on this thread before the read
        told = true;
        recovered(&mut told, &mut owed, &tx);
        assert_eq!(rx.try_recv(), Ok(AgentMessage::capture_ready())); // flake-ok: `recovered` sends on this thread before the read
        recovered(&mut told, &mut owed, &tx);
        assert!(rx.try_recv().is_err(), "told once"); // flake-ok: `recovered` sends on this thread before the read
        assert!(!told && !owed);
    }

    #[test]
    fn a_recovery_the_full_queue_refused_is_told_on_a_later_pass() {
        let (tx, mut rx) = mpsc::channel(1);
        tx.try_send(AgentMessage::hello()).unwrap();
        let (mut told, mut owed) = (true, false);
        recovered(&mut told, &mut owed, &tx);
        assert!(owed, "the queue was full");
        assert_eq!(rx.try_recv(), Ok(AgentMessage::hello())); // flake-ok: `recovered` sends on this thread before the read
        recovered(&mut told, &mut owed, &tx);
        assert!(!owed);
        assert_eq!(rx.try_recv(), Ok(AgentMessage::capture_ready())); // flake-ok: `recovered` sends on this thread before the read
    }
}
