// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use crate::cli;

#[test]
fn takes_exactly_one_endpoint() {
    assert_eq!(
        cli::parse(["ssh-bridge", r"\\.\pipe\puddle-0123"]),
        Ok(Command::SshBridge {
            endpoint: PathBuf::from(r"\\.\pipe\puddle-0123")
        })
    );
    assert_eq!(
        cli::parse(["ssh-bridge"]),
        Err(UsageError::MissingArgument("ssh-bridge <endpoint>"))
    );
    assert_eq!(
        cli::parse(["ssh-bridge", "a", "b"]),
        Err(UsageError::UnknownArgument("b".into()))
    );
}

#[test]
fn causes_are_chained() {
    let io = std::io::Error::other("os said no");
    let err = puddle_ssh::bridge::BridgeError::ServerRead(io);
    assert_eq!(
        with_causes(&err),
        "the connection to the workspace broke: os said no"
    );
}
