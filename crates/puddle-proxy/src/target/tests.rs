// SPDX-License-Identifier: AGPL-3.0-or-later
use puddle_netpolicy::normalise_host;

use super::*;

#[test]
fn host_header_omits_port_80_and_brackets_ipv6() {
    let t = |h: &str, port| Target {
        host: normalise_host(h).unwrap().into_host(),
        port,
    };
    assert_eq!(t("example.com", 80).host_header(), "example.com");
    assert_eq!(t("example.com", 8080).host_header(), "example.com:8080");
    assert_eq!(
        t("bücher.example", 80).host_header(),
        "xn--bcher-kva.example"
    );
    assert_eq!(t("::1", 80).host_header(), "[::1]");
    assert_eq!(t("::1", 8080).host_header(), "[::1]:8080");
    assert_eq!(t("10.0.0.1", 81).host_header(), "10.0.0.1:81");
}
