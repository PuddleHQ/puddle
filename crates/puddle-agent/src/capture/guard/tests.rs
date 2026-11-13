// SPDX-License-Identifier: AGPL-3.0-or-later
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
