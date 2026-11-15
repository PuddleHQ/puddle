// SPDX-License-Identifier: AGPL-3.0-or-later
use proptest::prelude::*;

use super::*;

#[test]
fn hello_carries_this_builds_version_and_protocol() {
    let line = AgentMessage::hello().to_line().unwrap();
    let text = String::from_utf8(line).unwrap();
    assert_eq!(
        text,
        format!(
            "{{\"type\":\"hello\",\"agent_version\":\"{}\",\"protocol\":1}}\n",
            puddle_types::VERSION
        )
    );
}

#[test]
fn an_unnamed_oom_kill_has_no_pid_or_process_fields() {
    let line = AgentMessage::oom_kill(None, None).to_line().unwrap();
    assert_eq!(line, b"{\"type\":\"oom_kill\"}\n");
    assert_eq!(
        AgentMessage::from_line(&line).unwrap(),
        AgentMessage::oom_kill(None, None)
    );
}

#[test]
fn a_ports_report_is_sorted_and_clean_and_always_fits_a_line() {
    assert_eq!(
        AgentMessage::ports([8080, 22, 8080, 0, 3000]),
        AgentMessage::Ports {
            listening: vec![22, 3000, 8080]
        }
    );
    let line = AgentMessage::ports([3000]).to_line().unwrap();
    assert_eq!(line, b"{\"type\":\"ports\",\"listening\":[3000]}\n");
    assert_eq!(
        AgentMessage::from_line(&line).unwrap(),
        AgentMessage::ports([3000])
    );
    let widest = AgentMessage::ports((60_000..).take(MAX_PORTS + 100));
    assert!(matches!(&widest, AgentMessage::Ports { listening } if listening.len() == MAX_PORTS));
    assert!(widest.to_line().unwrap().len() < MAX_LINE);
}

#[test]
fn unknown_types_and_fields_parse_as_unknown_or_are_ignored() {
    assert_eq!(
        AgentMessage::from_line(b"{\"type\":\"cpu_pressure\",\"x\":1}").unwrap(),
        AgentMessage::Unknown
    );
    assert_eq!(
        AgentMessage::from_line(b"{\"type\":\"oom_kill\",\"pid\":3,\"extra\":true}").unwrap(),
        AgentMessage::oom_kill(Some(3), None)
    );
}

#[test]
fn garbage_is_an_error_not_a_panic() {
    for bad in [
        &b""[..],
        b"{",
        b"[]",
        b"{\"pid\":1}",
        b"{\"type\":7}",
        b"{\"type\":\"oom_kill\",\"pid\":-1}",
        b"\xff\xfe",
    ] {
        assert!(
            matches!(AgentMessage::from_line(bad), Err(LineError::Json(_))),
            "{bad:?}"
        );
    }
}

#[test]
fn a_message_too_long_for_the_host_is_refused_by_the_writer() {
    let long = AgentMessage::oom_kill(Some(1), Some("x".repeat(MAX_LINE)));
    assert_eq!(long.to_line(), Err(LineError::TooLong));
    assert!(LineError::TooLong.to_string().contains("4096"));
}

async fn lines(input: &[u8], max: usize) -> Vec<io::Result<Vec<u8>>> {
    let mut reader = tokio::io::BufReader::with_capacity(3, input);
    let mut out = Vec::new();
    let mut line = Vec::new();
    loop {
        match read_line(&mut reader, &mut line, max).await {
            Ok(true) => out.push(Ok(line.clone())),
            Ok(false) => return out,
            Err(e) => {
                out.push(Err(e));
                return out;
            }
        }
    }
}

#[tokio::test]
async fn read_line_splits_on_newlines_across_buffer_refills() {
    let got = lines(b"ab\ncdefg\n\n", 16).await;
    let got: Vec<_> = got.into_iter().map(Result::unwrap).collect();
    assert_eq!(got, [&b"ab\n"[..], b"cdefg\n", b"\n"]);
}

#[tokio::test]
async fn read_line_refuses_a_line_past_the_limit() {
    let got = lines(b"abc\nabcdefgh\n", 5).await;
    assert_eq!(got.len(), 2);
    assert_eq!(got[0].as_ref().unwrap(), b"abc\n");
    assert_eq!(
        got[1].as_ref().unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    // Exactly at the limit is fine.
    assert_eq!(lines(b"abcd\n", 5).await[0].as_ref().unwrap(), b"abcd\n");
}

#[tokio::test]
async fn read_line_reports_a_stream_cut_inside_a_line() {
    let got = lines(b"ok\npartial", 64).await;
    assert_eq!(
        got[1].as_ref().unwrap_err().kind(),
        io::ErrorKind::UnexpectedEof
    );
}

#[test]
fn the_report_messages_have_a_fixed_shape_and_unknown_kinds_still_parse() {
    let exit = AgentMessage::entrypoint_exited(127, "x: not found".into());
    assert_eq!(
        exit.to_line().unwrap(),
        b"{\"type\":\"entrypoint_exited\",\"status\":127,\"tail\":\"x: not found\"}\n"
    );
    let dns = AgentMessage::stub_dns(StubDnsTrouble::NoStandInAddress, 3);
    assert_eq!(
        dns.to_line().unwrap(),
        b"{\"type\":\"stub_dns\",\"kind\":\"no_stand_in_address\",\"count\":3}\n"
    );
    for kind in StubDnsTrouble::ALL {
        let msg = AgentMessage::stub_dns(kind, 1);
        assert_eq!(
            AgentMessage::from_line(&msg.to_line().unwrap()).unwrap(),
            msg
        );
    }
    assert_eq!(
        AgentMessage::from_line(b"{\"type\":\"stub_dns\",\"kind\":\"cosmic_rays\",\"count\":1}")
            .unwrap(),
        AgentMessage::stub_dns(StubDnsTrouble::Unknown, 1)
    );
    assert_eq!(
        AgentMessage::from_line(b"{\"type\":\"entrypoint_exited\",\"status\":1}").unwrap(),
        AgentMessage::entrypoint_exited(1, String::new())
    );
    // A status no shell gives is not a message.
    assert!(AgentMessage::from_line(b"{\"type\":\"entrypoint_exited\",\"status\":256}").is_err());
}

#[test]
fn the_editor_status_round_trips_and_is_cut_to_its_limit() {
    let refused =
        AgentMessage::editor_status(EditorProblemKind::Install, Some("checksum mismatch"));
    assert_eq!(
        refused.to_line().unwrap(),
        b"{\"type\":\"editor_status\",\"kind\":\"install\",\"problem\":\"checksum mismatch\"}\n"
    );
    assert_eq!(
        AgentMessage::from_line(&refused.to_line().unwrap()).unwrap(),
        refused
    );
    let fine = AgentMessage::editor_status(EditorProblemKind::Install, None);
    assert_eq!(
        fine.to_line().unwrap(),
        b"{\"type\":\"editor_status\",\"kind\":\"install\"}\n"
    );
    assert_eq!(
        AgentMessage::from_line(&fine.to_line().unwrap()).unwrap(),
        fine
    );
    let long =
        AgentMessage::editor_status(EditorProblemKind::Install, Some(&"\u{1b}".repeat(5000)));
    let sent: serde_json::Value = serde_json::from_slice(&long.to_line().unwrap()).unwrap();
    let text = sent["problem"].as_str().unwrap();
    assert_eq!(text.chars().count(), MAX_EDITOR_PROBLEM_CHARS);
    assert!(long.to_line().is_ok(), "the cut text fits a control line");
}

#[test]
fn capture_messages_round_trip_and_the_recovery_has_no_fields() {
    assert_eq!(
        AgentMessage::capture_ready().to_line().unwrap(),
        b"{\"type\":\"capture_ready\"}\n"
    );
    for msg in [
        AgentMessage::capture_ready(),
        AgentMessage::capture_tampered(true),
        AgentMessage::capture_unavailable("no nf_tables".into()),
    ] {
        assert_eq!(
            AgentMessage::from_line(&msg.to_line().unwrap()).unwrap(),
            msg
        );
    }
}

#[test]
fn an_update_problem_names_its_kind_and_an_unknown_kind_still_parses() {
    let update = AgentMessage::editor_status(EditorProblemKind::Update, Some("rolled back"));
    assert_eq!(
        update.to_line().unwrap(),
        b"{\"type\":\"editor_status\",\"kind\":\"update\",\"problem\":\"rolled back\"}\n"
    );
    assert_eq!(
        AgentMessage::from_line(&update.to_line().unwrap()).unwrap(),
        update
    );
    let user = AgentMessage::editor_status(EditorProblemKind::SessionUser, Some("no agent"));
    assert_eq!(
        user.to_line().unwrap(),
        b"{\"type\":\"editor_status\",\"kind\":\"session_user\",\"problem\":\"no agent\"}\n"
    );
    assert_eq!(
        AgentMessage::from_line(&user.to_line().unwrap()).unwrap(),
        user
    );
    // A line from an agent from before the kind existed is an install problem.
    assert_eq!(
        AgentMessage::from_line(b"{\"type\":\"editor_status\",\"problem\":\"x\"}").unwrap(),
        AgentMessage::editor_status(EditorProblemKind::Install, Some("x"))
    );
    assert_eq!(
        AgentMessage::from_line(b"{\"type\":\"editor_status\",\"kind\":\"later\"}").unwrap(),
        AgentMessage::editor_status(EditorProblemKind::Unknown, None)
    );
}

proptest! {
    #[test]
    fn any_bytes_parse_or_fail_without_panicking(bytes in proptest::collection::vec(any::<u8>(), 0..256)) {
        let _ = AgentMessage::from_line(&bytes);
    }

    #[test]
    fn oom_kill_lines_round_trip(pid in proptest::option::of(any::<u32>()), name in proptest::option::of(".{0,64}")) {
        let msg = AgentMessage::oom_kill(pid, name);
        let line = msg.to_line().unwrap();
        prop_assert_eq!(line.split(|&b| b == b'\n').count(), 2);
        prop_assert_eq!(AgentMessage::from_line(&line).unwrap(), msg);
    }
}
