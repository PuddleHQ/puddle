// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn a_row_carries_the_short_name_the_display_name_and_the_time() {
    let row = kept_login_row(&puddle_logins::KeptLogin {
        profile: "claude",
        name: "Claude Code",
        since_ms: Some(1_761_480_000_000),
    });
    assert_eq!(
        row,
        KeptLogin {
            service: "claude".to_owned(),
            name: "Claude Code".to_owned(),
            since: Some(1_761_480_000_000),
        }
    );
}
