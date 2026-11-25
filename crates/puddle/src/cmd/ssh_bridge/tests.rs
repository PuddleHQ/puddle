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

#[test]
fn a_finished_session_is_reported_only_when_asked() {
    use puddle_ssh::bridge::BridgeError;
    let done: Result<_, BridgeError> = Ok("ServerClosed, sent 3");
    let mut quiet = Vec::new();
    assert_eq!(finish(&done, false, &mut quiet), ExitCode::SUCCESS);
    assert_eq!(quiet, Vec::<u8>::new());
    let mut loud = Vec::new();
    assert_eq!(finish(&done, true, &mut loud), ExitCode::SUCCESS);
    let line = String::from_utf8(loud).unwrap();
    assert_eq!(line, "puddle: ssh bridge ended: \"ServerClosed, sent 3\"\n");
    let mut failed = Vec::new();
    assert_eq!(
        finish(&Err::<(), _>(BridgeError::NotPuddle), false, &mut failed),
        ExitCode::FAILURE
    );
    assert_eq!(
        String::from_utf8(failed).unwrap(),
        "puddle: the SSH endpoint did not answer as Puddle does\n"
    );
}
