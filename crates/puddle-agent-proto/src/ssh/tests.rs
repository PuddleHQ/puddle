// SPDX-License-Identifier: AGPL-3.0-or-later
use proptest::prelude::*;

use super::*;

#[test]
fn real_client_identification_lines_match() {
    for line in [
        &b"SSH-2.0-OpenSSH_10.0p2 Debian-7\r\n"[..],
        b"SSH-2.0-libssh2_1.11.1\r\n",
        b"SSH-2.0-Go\r\n",
        b"SSH-2.0-paramiko_3.4.0\r\n",
        b"SSH-1.99-OpenSSH_3.9\r\n",
        b"SSH-2.0-x",
    ] {
        assert!(is_banner(line), "{}", String::from_utf8_lossy(line));
    }
}

#[test]
fn other_traffic_and_near_misses_do_not() {
    for bytes in [
        &b""[..],
        b"S",
        b"SSH-",
        b"SSH-x",
        b"ssh-2.0-lower",
        b" SSH-2.0-x",
        b"SSH2.0-x",
        b"puddle: SSH-2.0 is not\r\n",
        b"\x16\x03\x01\x02\x00\x01\x00\x01\xfc",
        b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n",
        b"CONNECT a.test:22 HTTP/1.1\r\n",
    ] {
        assert!(!is_banner(bytes), "{}", String::from_utf8_lossy(bytes));
    }
}

proptest! {
    #[test]
    fn matching_never_panics_and_needs_the_prefix(
        bytes in proptest::collection::vec(any::<u8>(), 0..16)
    ) {
        if is_banner(&bytes) {
            prop_assert!(bytes.starts_with(b"SSH-"));
        }
    }

    #[test]
    fn any_line_that_starts_like_a_banner_matches(
        digit in b'0'..=b'9',
        rest in proptest::collection::vec(any::<u8>(), 0..64)
    ) {
        let mut line = vec![b'S', b'S', b'H', b'-', digit];
        line.extend(rest);
        prop_assert!(is_banner(&line));
    }
}
