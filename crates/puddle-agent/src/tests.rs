// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn only_a_wildcard_or_equal_listener_covers_the_bridge_address() {
    let bridge: SocketAddr = "172.17.0.1:3128".parse().unwrap();
    assert!(covers("0.0.0.0:3128".parse().unwrap(), bridge));
    assert!(covers("172.17.0.1:3128".parse().unwrap(), bridge));
    assert!(!covers("0.0.0.0:3129".parse().unwrap(), bridge));
    assert!(!covers("127.0.0.1:3128".parse().unwrap(), bridge));
}
