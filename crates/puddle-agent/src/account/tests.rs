// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

const PASSWD: &str = "root:x:0:0:root:/root:/bin/bash\n\
        vscode:x:1000:1000::/home/vscode:/bin/bash\n\
        agent:x:1001:1001::/home/agent:\n";
const GROUP: &str = "root:x:0:\n\
        docker:x:998:vscode,agent\n\
        agent:x:1001:\n\
        audio:x:29:agent\n\
        broken\n";

#[test]
fn the_user_gets_its_ids_home_and_every_group_it_is_listed_in() {
    let agent = Account::find("agent", PASSWD, GROUP).unwrap();
    assert_eq!(
        agent,
        Account {
            name: "agent".to_owned(),
            uid: 1001,
            gid: 1001,
            groups: vec![1001, 998, 29],
            home: "/home/agent".to_owned(),
            shell: "/bin/sh".to_owned(),
        }
    );
    assert_eq!(
        agent.env(),
        [
            ("HOME", "/home/agent"),
            ("USER", "agent"),
            ("LOGNAME", "agent"),
            ("SHELL", "/bin/sh"),
        ]
    );
    let vscode = Account::find("vscode", PASSWD, "").unwrap();
    assert_eq!(
        (vscode.groups, vscode.shell.as_str()),
        (vec![1000], "/bin/bash")
    );
}

#[test]
fn a_missing_or_unusable_entry_is_no_account() {
    assert_eq!(Account::find("nobody", PASSWD, GROUP), None);
    assert_eq!(
        Account::find("agent", "agent:x:one:1001::/h:/bin/sh\n", ""),
        None
    );
    assert_eq!(Account::find("agent", "agent:x:1001\n", ""), None);
    // A name that is only a prefix of an entry is not that entry.
    assert_eq!(Account::find("age", PASSWD, GROUP), None);
}

#[cfg(target_os = "linux")]
#[test]
fn this_systems_root_is_read_and_a_user_it_lacks_is_not_found() {
    let root = Account::read("root").unwrap();
    assert_eq!((root.uid, root.gid, root.groups[0]), (0, 0, 0));
    let missing = Account::read("puddle-no-such-user").unwrap_err();
    assert_eq!(missing.kind(), io::ErrorKind::NotFound);
    assert!(
        missing.to_string().contains("puddle-no-such-user"),
        "{missing}"
    );
}
