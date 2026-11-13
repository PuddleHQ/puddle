// SPDX-License-Identifier: AGPL-3.0-or-later
use super::overlap;

#[test]
fn patterns_overlap_when_they_name_a_host_in_common() {
    assert!(overlap("api.example.com", "api.example.com"));
    assert!(!overlap("api.example.com", "web.example.com"));
    assert!(overlap("*.example.com", "api.example.com"));
    assert!(overlap("api.example.com", "*.example.com"));
    assert!(
        !overlap("*.example.com", "example.com"),
        "a wildcard is below"
    );
    assert!(!overlap("example.com", "*.example.com"));
    assert!(!overlap("*.example.com", "badexample.com"));
    assert!(overlap("*.example.com", "*.example.com"));
    assert!(overlap("*.example.com", "*.api.example.com"));
    assert!(overlap("*.api.example.com", "*.example.com"));
    assert!(!overlap("*.example.com", "*.example.org"));
}
