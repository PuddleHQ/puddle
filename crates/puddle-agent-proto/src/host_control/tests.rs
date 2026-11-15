// SPDX-License-Identifier: AGPL-3.0-or-later
use proptest::prelude::*;

use super::*;

#[test]
fn the_preamble_is_the_registered_kind() {
    assert_eq!(PREAMBLE, b"\0puddle-host-control/1\n");
    assert!(PREAMBLE.len() <= crate::kind::MAX_PREAMBLE);
}

#[test]
fn a_message_and_a_reply_round_trip_as_one_line() {
    let line = HostMessage::EndExtensionHosts.to_line();
    assert_eq!(line, b"{\"type\":\"end_extension_hosts\"}\n");
    assert_eq!(
        HostMessage::from_line(&line),
        Ok(HostMessage::EndExtensionHosts)
    );
    let reply = HostReply::done(1, 6, 0);
    assert_eq!(
        reply.to_line(),
        b"{\"type\":\"done\",\"hosts\":1,\"processes\":6,\"orphans\":0}\n"
    );
    assert_eq!(HostReply::from_line(&reply.to_line()), Ok(reply));
    assert_eq!(
        HostReply::from_line(&HostReply::Ignored.to_line()),
        Ok(HostReply::Ignored)
    );
}

#[test]
fn the_switch_message_and_its_reply_round_trip() {
    let message = HostMessage::SwitchEditorBuild;
    assert_eq!(message.to_line(), b"{\"type\":\"switch_editor_build\"}\n");
    assert_eq!(HostMessage::from_line(&message.to_line()), Ok(message));
    for switched in [false, true] {
        let reply = HostReply::EditorBuild { switched };
        assert_eq!(HostReply::from_line(&reply.to_line()), Ok(reply));
    }
}

#[test]
fn unknown_types_parse_as_unknown_and_extra_fields_are_ignored() {
    assert_eq!(
        HostMessage::from_line(b"{\"type\":\"drop_caches\",\"x\":1}"),
        Ok(HostMessage::Unknown)
    );
    assert_eq!(
        HostReply::from_line(b"{\"type\":\"burnt\"}"),
        Ok(HostReply::Unknown)
    );
    // A reply from an agent that predates `orphans` still reads.
    assert_eq!(
        HostReply::from_line(b"{\"type\":\"done\",\"hosts\":1,\"processes\":2,\"later\":true}"),
        Ok(HostReply::done(1, 2, 0))
    );
}

#[test]
fn garbage_is_an_error_not_a_panic() {
    for bad in [&b""[..], b"{", b"[]", b"{\"type\":7}", b"\xff\xfe"] {
        assert!(HostMessage::from_line(bad).is_err(), "{bad:?}");
        assert!(HostReply::from_line(bad).is_err(), "{bad:?}");
    }
}

proptest! {
    #[test]
    fn any_bytes_parse_or_fail_without_panicking(bytes in proptest::collection::vec(any::<u8>(), 0..256)) {
        let _ = HostMessage::from_line(&bytes);
        let _ = HostReply::from_line(&bytes);
    }

    #[test]
    fn replies_round_trip(hosts: u32, processes: u32, orphans: u32) {
        let reply = HostReply::done(hosts, processes, orphans);
        let line = reply.to_line();
        prop_assert!(line.len() <= MAX_LINE);
        prop_assert_eq!(HostReply::from_line(&line), Ok(reply));
    }
}
