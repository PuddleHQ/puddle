// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn arguments_map_to_commands() {
    assert_eq!(parse::<&str>(&[]), Command::Run);
    assert_eq!(parse(&["--version"]), Command::Version);
    assert_eq!(parse(&["-V"]), Command::Version);
    assert!(matches!(
        parse(&["connect", "h", "22"]),
        Command::Connect(_)
    ));
    assert!(matches!(
        parse(&["connect", "h", "22", "alias"]),
        Command::Connect(_)
    ));
    for bad in [
        &["connect"][..],
        &["connect", "h"],
        &["connect", "h", "0"],
        &["connect", "h", "x"],
    ] {
        assert!(matches!(parse(bad), Command::Usage(_)), "{bad:?}");
    }
    let Command::Usage(msg) = parse(&["--help"]) else {
        panic!("not usage");
    };
    assert!(msg.contains("--help") && msg.contains(USAGE));
    assert!(matches!(parse(&["--version", "x"]), Command::Usage(_)));
    assert!(matches!(
        parse(&["merge-file", "forget", "/s", "/g"]),
        Command::MergeFile(_)
    ));
    assert!(matches!(parse(&["merge-file"]), Command::Usage(_)));
    assert_eq!(
        parse(&["mise-fetch", "/workspaces/w/.puddle/mise"]),
        Command::MiseFetch(mise::Request {
            dir: "/workspaces/w/.puddle/mise".into()
        })
    );
    for bad in [
        &["mise-fetch"][..],
        &["mise-fetch", ""],
        &["mise-fetch", "a", "b"],
        &["mise-install"],
        &["mise-install", "/workspaces/w", "--force"],
    ] {
        assert!(matches!(parse(bad), Command::Usage(_)), "{bad:?}");
    }
    assert_eq!(
        parse(&["mise-install", "/workspaces/w", "--missing"]),
        Command::MiseInstall(mise_install::Request {
            root: "/workspaces/w".into(),
            report_only: true,
        })
    );
}
