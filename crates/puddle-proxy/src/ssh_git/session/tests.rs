// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn a_git_protocol_value_is_a_list_of_the_three_versions_and_nothing_else() {
    assert_eq!(protocol_versions("version=2"), Some(vec![2]));
    assert_eq!(protocol_versions("version=1:version=2"), Some(vec![1, 2]));
    assert_eq!(protocol_versions("version=0"), Some(vec![0]));
    for bad in [
        "",
        "version=3",
        "version=2:",
        "version=2:foo",
        "VERSION=2",
        "version=2 ",
        "version=0:version=1:version=2:version=2",
        "version=2\nx=1",
    ] {
        assert_eq!(protocol_versions(bad), None, "{bad:?}");
    }
}

#[test]
fn version_0_is_the_absence_of_a_git_protocol() {
    assert_eq!(git_protocol(&[]), None);
    assert_eq!(git_protocol(&[0]), None);
    assert_eq!(git_protocol(&[2]).as_deref(), Some("version=2"));
    assert_eq!(
        git_protocol(&[0, 1, 2]).as_deref(),
        Some("version=1:version=2")
    );
}

#[test]
fn libssh2_is_told_by_its_name_anywhere_in_what_the_guest_sent_first() {
    assert!(names_libssh2(b"SSH-2.0-libssh2_1.11.0\r\n"));
    assert!(names_libssh2(b"xxlibssh2"));
    assert!(!names_libssh2(b"SSH-2.0-OpenSSH_9.6\r\n"));
    assert!(!names_libssh2(b"libssh"));
    assert!(!names_libssh2(b""));
}

#[test]
fn the_server_answers_as_puddle_with_no_delays_and_no_idle_timeout() {
    let config = server_config(Vec::new());
    assert!(
        matches!(&config.server_id, russh::SshId::Standard(id) if id == "SSH-2.0-Puddle"),
        "{:?}",
        config.server_id
    );
    assert_eq!(config.auth_rejection_time, std::time::Duration::ZERO);
    assert_eq!(
        config.auth_rejection_time_initial,
        Some(std::time::Duration::ZERO)
    );
    assert_eq!(config.inactivity_timeout, None);
}

fn event() -> ConnectionEvent {
    let request = EgressRequest::new(
        WorkspaceId::new("box").unwrap(),
        puddle_types::Host::parse_normalised("bound.test").unwrap(),
        22,
    );
    ConnectionEvent::new(&request, ConnectionDecision::Allow, ConnectionReason::Rule)
}

fn leg(credentials: &[&str]) -> Outcome {
    Outcome {
        foreign_credentials: credentials.iter().map(|c| (*c).to_owned()).collect(),
        ..Outcome::default()
    }
}

#[test]
fn a_connection_with_no_closed_leg_keeps_its_record_as_it_was() {
    let mut record = event();
    apply_legs(&mut record, &[]);
    assert_eq!(record, event());
}

#[test]
fn the_legs_credentials_and_stand_ins_join_the_record_once_each_up_to_the_cap() {
    let mut record = event();
    let many: Vec<String> = (0..MAX_UNSWAPPED_STAND_INS + 4)
        .map(|n| format!("kind{n}"))
        .collect();
    let many: Vec<&str> = many.iter().map(String::as_str).collect();
    apply_legs(
        &mut record,
        &[leg(&["a", "b"]), leg(&["b", "c"]), leg(&many)],
    );
    assert_eq!(&record.foreign_credentials[..3], ["a", "b", "c"]);
    assert_eq!(record.foreign_credentials.len(), MAX_UNSWAPPED_STAND_INS);
    let stand_ins = Outcome {
        unswapped_stand_ins: vec!["s1".to_owned()],
        scrubbed_stand_ins: vec!["s2".to_owned()],
        answer_credentials: vec!["t".to_owned()],
        ..Outcome::default()
    };
    let mut record = event();
    apply_legs(&mut record, &[stand_ins]);
    assert_eq!(record.unswapped_stand_ins, ["s1"]);
    assert_eq!(record.scrubbed_stand_ins, ["s2"]);
    assert_eq!(record.answer_credentials, ["t"]);
}

#[test]
fn a_leg_that_injected_a_credential_marks_the_record_and_the_first_binding_stays() {
    let mut record = event();
    let bound = |id: &str| Outcome {
        injected: true,
        binding_id: Some(id.to_owned()),
        ..Outcome::default()
    };
    let (first, second) = (bound("one"), bound("two"));
    apply_legs(&mut record, &[Outcome::default(), first, second]);
    assert!(record.injected);
    assert_eq!(record.binding_id.as_deref(), Some("one"));
}

#[test]
fn the_first_leg_gives_the_address_hop_and_connection_outcome_not_a_request_line() {
    let mut record = event();
    let first = Outcome {
        resolved_ip: Some("192.0.2.1".parse().unwrap()),
        hop: Some("corp-proxy".to_owned()),
        connect: Some(puddle_types::ConnectionOutcome::Connected),
        http: Some(HttpRequestLine::new("POST", "/x")),
        ..Outcome::default()
    };
    let second = Outcome {
        resolved_ip: Some("192.0.2.2".parse().unwrap()),
        ..Outcome::default()
    };
    apply_legs(&mut record, &[first, second]);
    assert_eq!(record.resolved_ip, Some("192.0.2.1".parse().unwrap()));
    assert_eq!(record.upstream.as_deref(), Some("corp-proxy"));
    assert_eq!(
        record.outcome,
        Some(puddle_types::ConnectionOutcome::Connected)
    );
    assert!(record.http.is_none(), "the record names the SSH command");
}

#[test]
fn a_connection_keeps_at_most_the_cap_of_closed_legs_and_the_first_ones() {
    let mut legs = Vec::new();
    for n in 0..super::super::MAX_LEGS + 3 {
        super::super::keep_leg(
            &mut legs,
            Outcome {
                binding_id: Some(n.to_string()),
                ..Outcome::default()
            },
        );
    }
    assert_eq!(legs.len(), super::super::MAX_LEGS);
    assert_eq!(legs[0].binding_id.as_deref(), Some("0"));
    assert_eq!(
        legs[super::super::MAX_LEGS - 1].binding_id,
        Some((super::super::MAX_LEGS - 1).to_string())
    );
}
