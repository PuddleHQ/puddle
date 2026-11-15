// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn the_header_value_is_built_per_scheme_and_is_marked_sensitive() {
    let token = Arc::new(Secret::new("CANARY-token".to_owned()));
    let bearer = Authorization::Bearer(token.clone()).header_value().unwrap();
    assert_eq!(bearer.to_str().unwrap(), "Bearer CANARY-token");
    assert!(bearer.is_sensitive());
    let basic = Authorization::BasicPassword(token).header_value().unwrap();
    assert_eq!(
        basic.to_str().unwrap(),
        format!("Basic {}", STANDARD.encode(":CANARY-token"))
    );
}

#[test]
fn a_token_a_header_cannot_carry_gives_no_header_and_debug_never_shows_it() {
    let bad = Arc::new(Secret::new("CANARY-line\nbreak".to_owned()));
    let auth = Authorization::Bearer(bad);
    assert!(auth.header_value().is_none());
    assert_eq!(format!("{auth:?}"), "Authorization::Bearer(<redacted>)");
    assert_eq!(
        format!(
            "{:?}",
            Authorization::BasicPassword(Arc::new(Secret::new("CANARY-x".to_owned())))
        ),
        "Authorization::BasicPassword(<redacted>)"
    );
}

#[test]
fn a_header_is_found_by_its_lower_case_name() {
    let reply = ApiReply::new(200, "[]").with_header("Link", "<x>");
    assert_eq!(reply.header("link"), Some("<x>"));
    assert_eq!(reply.header("etag"), None);
    assert_eq!(reply.body, b"[]");
}
