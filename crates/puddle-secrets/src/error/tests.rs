// SPDX-License-Identifier: AGPL-3.0-or-later
use super::Tool;

#[test]
fn a_tool_is_named_the_way_the_user_knows_it() {
    assert_eq!(
        (
            Tool::Gh.name(),
            Tool::Git.name(),
            Tool::Az.name(),
            Tool::SystemdCreds.name(),
            Tool::Loginctl.name()
        ),
        ("gh", "git", "az", "systemd-creds", "loginctl")
    );
}
