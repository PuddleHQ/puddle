// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn the_code_and_address_must_have_the_shape_gh_prints() {
    assert_eq!(valid_code(" ABCD-1234 "), Some("ABCD-1234".into()));
    for bad in ["", "AB", "ABCD 1234", "ABCD-1234-ABCD-1234", "AB<script>"] {
        assert_eq!(valid_code(bad), None, "{bad:?}");
    }
    let host = HostName::new("github.com").unwrap();
    assert_eq!(
        valid_url(" https://github.com/login/device ", &host),
        Some("https://github.com/login/device".into())
    );
    assert_eq!(
        valid_url("https://github.com", &host),
        Some("https://github.com".into())
    );
    for bad in [
        "http://github.com/login/device",
        "https://evil.example/login/device",
        "https://github.com.evil.example/x",
        "https://github.com/a b",
        "javascript:alert(1)",
    ] {
        assert_eq!(valid_url(bad, &host), None, "{bad:?}");
    }
}
