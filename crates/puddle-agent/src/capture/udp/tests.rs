// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn ip(last: u8) -> IpAddr {
    IpAddr::from([93, 184, 216, last])
}

#[test]
fn the_first_datagram_to_a_destination_is_reported_and_repeats_are_counted() {
    let mut r = Reporter::default();
    let t0 = Instant::now();
    assert_eq!(
        r.refused(t0, ip(1), 443),
        Some(AgentMessage::udp_refused("93.184.216.1".into(), 443, 1))
    );
    for i in 1..=4 {
        assert_eq!(r.refused(t0 + Duration::from_secs(i), ip(1), 443), None);
    }
    let after = r.refused(t0 + QUIET, ip(1), 443);
    assert_eq!(
        after,
        Some(AgentMessage::udp_refused("93.184.216.1".into(), 443, 5))
    );
}

#[test]
fn another_port_or_address_is_its_own_destination() {
    let mut r = Reporter::default();
    let t0 = Instant::now();
    assert!(r.refused(t0, ip(1), 443).is_some());
    assert!(r.refused(t0, ip(1), 123).is_some());
    assert!(r.refused(t0, ip(2), 443).is_some());
}

#[test]
fn a_spray_of_destinations_is_cut_to_a_few_reports_a_second() {
    let mut r = Reporter::default();
    let t0 = Instant::now();
    let sent = (0..200u8)
        .filter(|i| r.refused(t0, ip(*i), 9).is_some())
        .count();
    assert_eq!(sent, PER_SECOND);
    // The next second reports the next ones; the count of the unreported is not lost.
    let later = t0 + Duration::from_secs(1);
    assert!(r.refused(later, ip(250), 9).is_some());
}

#[test]
fn the_table_of_destinations_stays_bounded() {
    let mut r = Reporter::default();
    let t0 = Instant::now();
    for i in 0..(u32::try_from(TRACKED).unwrap() + 500) {
        let dest = IpAddr::from(i.to_be_bytes());
        let _ = r.refused(t0, dest, 1);
    }
    assert!(r.seen.len() <= TRACKED);
}
