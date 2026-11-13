// SPDX-License-Identifier: AGPL-3.0-or-later
use microsandbox::protocol::exec::{ExecFailed, ExecFailureKind};

use super::*;

fn failed(kind: ExecFailureKind) -> ExecFailed {
    ExecFailed {
        kind,
        errno: Some(2),
        errno_name: Some("ENOENT".into()),
        message: "nope: not found".into(),
        stage: Some("execvp".into()),
    }
}

#[test]
fn spawn_failures_read_like_a_shell() {
    let out = spawn_failure(&failed(ExecFailureKind::NotFound)).unwrap();
    assert_eq!(out.status.code, 127);
    assert_eq!(out.stderr_text(), "nope: not found\n");
    for kind in [
        ExecFailureKind::PermissionDenied,
        ExecFailureKind::NotExecutable,
    ] {
        assert_eq!(spawn_failure(&failed(kind)).unwrap().status.code, 126);
    }
    for kind in [
        ExecFailureKind::BadCwd,
        ExecFailureKind::BadArgs,
        ExecFailureKind::ResourceLimit,
        ExecFailureKind::UserSetupFailed,
    ] {
        assert!(spawn_failure(&failed(kind)).is_none());
    }
}

#[test]
fn a_stop_the_vm_did_not_answer_is_ended_by_a_kill_and_any_other_failure_is_not() {
    use microsandbox::agent::AgentClientError;
    let unanswered = [
        MicrosandboxError::StopTimeout {
            name: "box".into(),
            identity: "box#1".into(),
            timeout: Duration::from_secs(30),
        },
        MicrosandboxError::SandboxStopTimedOut {
            name: "box".into(),
            timeout: Duration::from_secs(30),
        },
        AgentClientError::Handshake("read id range: timed out before relay sent bytes".into())
            .into(),
        AgentClientError::Connect {
            // flake-ok: a value that is parsed or compared and never opened, so the missing drive letter does not matter
            path: "/no/relay.sock".into(),
            source: std::io::Error::other("refused"),
        }
        .into(),
    ];
    for e in &unanswered {
        assert!(stop_went_unanswered(e), "{e}");
    }
    for e in [
        MicrosandboxError::SandboxNotFound("box".into()),
        MicrosandboxError::Runtime("vmm gone".into()),
        AgentClientError::SandboxNotFound("box".into()).into(),
    ] {
        assert!(!stop_went_unanswered(&e), "{e}");
    }
}
