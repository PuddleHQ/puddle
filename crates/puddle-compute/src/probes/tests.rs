// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;

#[test]
fn a_probe_is_found_by_its_marker_and_hands_over_the_arguments_after_its_script() {
    let request = ExecRequest::new("perl", ["-e", ICMP_PROBE, "127.0.0.1", "1.1.1.1"]);
    assert_eq!(
        Probe::of(&request),
        Some((Probe::Icmp, vec!["127.0.0.1", "1.1.1.1"]))
    );
    assert_eq!(
        Probe::of(&ExecRequest::sh(NO_DIRECT_NETWORK)).map(|p| p.0),
        Some(Probe::NoDirectNetwork)
    );
    assert_eq!(
        Probe::of(&ExecRequest::new("perl", ["-e", PACKET_PROBE])).map(|p| p.0),
        Some(Probe::Packet)
    );
    assert_eq!(
        Probe::of(&ExecRequest::new("perl", ["-e", LOCAL_CID])).map(|p| p.0),
        Some(Probe::LocalCid)
    );
    assert_eq!(
        Probe::of(&ExecRequest::new("perl", ["-e", VSOCK_SWEEP, "m", "2"])).map(|p| p.0),
        Some(Probe::VsockSweep)
    );
}

#[test]
fn other_requests_and_unknown_marker_names_are_no_probe() {
    assert_eq!(Probe::of(&ExecRequest::sh("exit 0")), None);
    let unknown = ExecRequest::sh("# puddle-probe: made-up\nexit 0");
    assert_eq!(Probe::of(&unknown), None);
}
