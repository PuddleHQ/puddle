// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn host(pattern: &'static str) -> puddle_store::SystemHost {
    puddle_store::SystemHost {
        pattern,
        note: "",
        reason: puddle_store::SystemReason::CodeServer,
        workspace: None,
    }
}

#[test]
fn a_host_whose_reason_is_unknown_is_left_out_and_counted() {
    let hosts = vec![host("a.example.com"), host("b.example.com")];
    let (shown, left_out) = wire_system_hosts(hosts, |h| {
        if h.pattern == "b.example.com" {
            None
        } else {
            SystemManagedHost::from_store(h)
        }
    });
    assert_eq!(left_out, 1);
    assert_eq!(shown.len(), 1);
    assert_eq!(shown[0].pattern, "a.example.com");
}

#[test]
fn hosts_with_known_reasons_are_all_shown() {
    let (shown, left_out) =
        wire_system_hosts(vec![host("a.example.com")], SystemManagedHost::from_store);
    assert_eq!(left_out, 0);
    assert_eq!(shown.len(), 1);
}
