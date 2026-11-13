// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use crate::testing::{TestKey, packet, server_kex, string};

fn seen(host: &TestKey, signature: &[u8]) -> SeenExchange {
    SeenExchange {
        host_key: host.key_data(),
        signature: signature.to_vec(),
    }
}

#[test]
fn the_reply_of_a_key_exchange_gives_its_host_key_and_signature() {
    let host = TestKey::ed25519(5);
    let signature = host.sign(b"exchange hash");
    let mut observer = KexObserver::new();
    assert_eq!(
        observer.feed(&server_kex(&host, &signature)),
        Some(seen(&host, &signature))
    );
    assert!(observer.is_done());
    assert_eq!(observer.feed(b"more"), None);
}

#[test]
fn bytes_that_arrive_one_at_a_time_give_the_same_answer() {
    let host = TestKey::ed25519(5);
    let signature = host.sign(b"exchange hash");
    let bytes = server_kex(&host, &signature);
    let mut observer = KexObserver::new();
    let mut found = Vec::new();
    for byte in &bytes {
        found.extend(observer.feed(std::slice::from_ref(byte)));
    }
    assert_eq!(found, vec![seen(&host, &signature)]);
}

#[test]
fn lines_before_the_identification_are_passed_over() {
    let host = TestKey::ed25519(5);
    let signature = host.sign(b"h");
    let mut bytes = b"Welcome\r\nbanner line two\n".to_vec();
    bytes.extend(server_kex(&host, &signature));
    assert_eq!(
        KexObserver::new().feed(&bytes),
        Some(seen(&host, &signature))
    );
}

#[test]
fn the_group_exchange_group_message_is_not_taken_for_the_reply() {
    let host = TestKey::ed25519(5);
    let signature = host.sign(b"h");
    let mut bytes = b"SSH-2.0-X\r\n".to_vec();
    // KEX_DH_GEX_GROUP is message 31 with two numbers.
    bytes.extend(packet(
        &[vec![31], string(&[0x7f; 64]), string(&[2])].concat(),
    ));
    // KEX_DH_GEX_REPLY is message 33.
    let reply = [
        vec![33],
        string(&host.blob()),
        string(&[9; 64]),
        string(&signature),
    ]
    .concat();
    bytes.extend(packet(&reply));
    assert_eq!(
        KexObserver::new().feed(&bytes),
        Some(seen(&host, &signature))
    );
}

#[test]
fn a_reply_whose_key_or_signature_does_not_parse_is_passed_over_until_newkeys() {
    let host = TestKey::ed25519(5);
    let signature = host.sign(b"h");
    for reply in [
        [
            vec![31],
            string(b"not a key"),
            string(&[1]),
            string(&signature),
        ]
        .concat(),
        [
            vec![31],
            string(&host.blob()),
            string(&[1]),
            string(b"not a signature"),
        ]
        .concat(),
        [
            vec![31],
            string(&host.blob()),
            string(&[1]),
            string(&signature),
            vec![0],
        ]
        .concat(),
        [vec![31], string(&host.blob()), string(&[1])].concat(),
        [vec![31], string(&host.blob())].concat(),
        vec![31],
    ] {
        let mut bytes = b"SSH-2.0-X\r\n".to_vec();
        bytes.extend(packet(&reply));
        let mut observer = KexObserver::new();
        assert_eq!(observer.feed(&bytes), None);
        assert!(!observer.is_done(), "{reply:?}");
        assert_eq!(observer.feed(&packet(&[21])), None);
        assert!(observer.is_done());
    }
}

#[test]
fn newkeys_before_any_reply_ends_the_observation() {
    let host = TestKey::ed25519(5);
    let signature = host.sign(b"h");
    let mut bytes = b"SSH-2.0-X\r\n".to_vec();
    bytes.extend(packet(&[21]));
    bytes.extend(server_kex(&host, &signature));
    let mut observer = KexObserver::new();
    assert_eq!(observer.feed(&bytes), None);
    assert!(observer.is_done());
}

#[test]
fn a_packet_that_cannot_be_read_ends_the_observation_with_nothing_seen() {
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("an empty packet", vec![0, 0, 0, 0]),
        ("padding longer than the packet", vec![0, 0, 0, 2, 9, 31]),
        ("no payload", vec![0, 0, 0, 1, 0]),
        (
            "a length over the limit",
            (u32::try_from(MAX_OBSERVED).unwrap() + 1)
                .to_be_bytes()
                .to_vec(),
        ),
    ];
    for (why, packet) in cases {
        let mut observer = KexObserver::new();
        let mut bytes = b"SSH-2.0-X\r\n".to_vec();
        bytes.extend(packet);
        assert_eq!(observer.feed(&bytes), None, "{why}");
        assert!(observer.is_done(), "{why}");
    }
}

#[test]
fn a_packet_still_arriving_waits_for_its_rest() {
    let mut observer = KexObserver::new();
    assert_eq!(observer.feed(b"SSH-2.0-X\r\n\0\0\0\x10\x04"), None);
    assert!(!observer.is_done());
    assert_eq!(observer.feed(b"\0\0"), None);
    assert!(!observer.is_done());
}

#[test]
fn a_length_exactly_at_the_limit_still_waits() {
    let mut observer = KexObserver::new();
    let mut bytes = b"SSH-2.0-X\r\n".to_vec();
    bytes.extend(u32::try_from(MAX_OBSERVED).unwrap().to_be_bytes());
    assert_eq!(observer.feed(&bytes), None);
    assert!(!observer.is_done());
}

#[test]
fn a_server_that_never_sends_a_line_end_or_too_much_is_given_up_on() {
    let mut observer = KexObserver::new();
    assert_eq!(observer.feed(&[b'x'; MAX_LINE]), None);
    assert!(
        !observer.is_done(),
        "a line of exactly the limit may still end"
    );
    assert_eq!(observer.feed(b"x"), None);
    assert!(observer.is_done());

    let mut long = vec![b'x'; MAX_LINE + 1];
    long.push(b'\n');
    let mut observer = KexObserver::new();
    assert_eq!(observer.feed(&long), None);
    assert!(observer.is_done());

    let mut at_limit = vec![b'x'; MAX_LINE];
    at_limit.push(b'\n');
    let mut observer = KexObserver::new();
    assert_eq!(observer.feed(&at_limit), None);
    assert!(!observer.is_done());

    let mut observer = KexObserver::new();
    assert_eq!(observer.feed(b"SSH-2.0-X\n"), None);
    let mut big = u32::try_from(MAX_OBSERVED).unwrap().to_be_bytes().to_vec();
    big.resize(MAX_OBSERVED, 4);
    assert_eq!(observer.feed(&big), None);
    assert!(!observer.is_done(), "exactly the limit is still read");
    assert_eq!(observer.feed(&[0]), None);
    assert!(observer.is_done());
}

#[test]
fn other_packets_before_the_reply_are_passed_over() {
    let host = TestKey::ed25519(5);
    let signature = host.sign(b"h");
    let mut bytes = b"SSH-2.0-X\n".to_vec();
    bytes.extend(packet(&[2, 0, 0, 0, 0])); // IGNORE
    bytes.extend(packet(&[20; 30])); // KEXINIT
    let reply = [
        vec![31],
        string(&host.blob()),
        string(&[1]),
        string(&signature),
    ]
    .concat();
    bytes.extend(packet(&reply));
    assert_eq!(
        KexObserver::new().feed(&bytes),
        Some(seen(&host, &signature))
    );
}

#[test]
fn a_large_key_exchange_in_one_read_is_seen() {
    // A post-quantum hybrid exchange sends kilobytes before the reply; it all fits the limit.
    let host = TestKey::ed25519(5);
    let signature = host.sign(b"h");
    let kex = server_kex(&host, &signature);
    let split = kex.iter().position(|&b| b == b'\n').unwrap() + 1;
    let mut bytes = kex[..split].to_vec();
    bytes.extend(packet(&[20; 8 * 1024]));
    bytes.extend_from_slice(&kex[split..]);
    assert!(bytes.len() < MAX_OBSERVED);
    assert_eq!(
        KexObserver::new().feed(&bytes),
        Some(seen(&host, &signature))
    );
}
