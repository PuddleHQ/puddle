// SPDX-License-Identifier: AGPL-3.0-or-later
use proptest::prelude::*;

use super::*;

#[test]
fn reverse_zones_and_address_shaped_names_stay_in_the_sandbox() {
    for name in [
        "",
        "4.3.2.1.in-addr.arpa",
        "1.0.0.127.ip6.arpa",
        "1.2.3.4",
        "example.123",
    ] {
        assert!(is_local_only(name), "{name:?}");
    }
}

#[test]
fn ordinary_names_go_to_the_host() {
    for name in [
        "example.com",
        "api.github.com",
        "printer",
        "localhost",
        "host.local",
        "metadata.google.internal",
        "db.default.svc.cluster.local",
        "xn--bcher-kva.example",
        "x.localdomain.example",
        "notlocal.example",
        "_mongodb._tcp.db.example.net",
        "a1.b2.example",
    ] {
        assert!(!is_local_only(name), "{name:?}");
        assert!(is_plain(name), "{name:?}");
    }
}

#[test]
fn a_name_that_is_not_plain_never_goes_to_the_host() {
    for name in [
        "",
        ".",
        "a..b",
        "a.b.",
        "Upper.example",
        "sp ace.example",
        "esc\u{1b}[2K.example",
        "bücher.example",
        "*.example",
        &"a".repeat(64),
        &format!("{0}.{0}.{0}.{0}", "a".repeat(63)),
    ] {
        assert!(!is_plain(name), "{name:?}");
    }
}

proptest! {
    #[test]
    fn classification_never_panics(name in ".{0,300}") {
        let _ = is_local_only(&name);
        let _ = is_plain(&name);
    }
}
