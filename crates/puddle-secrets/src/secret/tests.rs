// SPDX-License-Identifier: AGPL-3.0-or-later
use super::Secret;

#[test]
fn debug_never_shows_the_value() {
    let s = Secret::new("CANARY-value".to_owned());
    assert_eq!(format!("{s:?}"), "<redacted>");
    assert_eq!(s.expose(), "CANARY-value");
}
