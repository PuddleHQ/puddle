// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn only_the_workspaces_started_on_microsoft_s_server_are_listed_and_a_stop_removes_one() {
    let flavours = Flavours::default();
    let name = |s: &str| WorkspaceId::new(s).unwrap();
    let microsoft = EditorServer::microsoft("/data", "/mnt", 300, false, true);
    flavours.running(&name("ms"), Some(&microsoft));
    flavours.running(&name("cs"), Some(&EditorServer::new("/data", "/mnt", 300)));
    flavours.running(&name("none"), None);
    assert_eq!(flavours.on_microsoft(), [name("ms")]);
    flavours.stopped(&name("ms"));
    assert_eq!(flavours.on_microsoft().len(), 0);
}
