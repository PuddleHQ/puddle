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
}
