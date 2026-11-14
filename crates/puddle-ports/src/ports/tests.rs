// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[tokio::test]
async fn ended_sessions_wait_for_ever_once_the_ports_are_gone() {
    let (tx, rx) = tokio::sync::broadcast::channel(4);
    let mut ended = EndedSessions(rx);
    drop(tx);
    let waited = tokio::time::timeout(Duration::from_millis(50), ended.next()).await;
    assert!(waited.is_err());
}

struct Names(Option<&'static str>);

impl ProgramLookup for Names {
    fn program(&self, _: u16) -> Option<String> {
        self.0.map(str::to_owned)
    }
}

#[test]
fn a_bind_refused_for_a_taken_port_names_the_program_that_has_it() {
    let err = std::io::Error::from(std::io::ErrorKind::AddrInUse);
    let said = bind_failure(8080, &err, &Names(Some("nginx")));
    assert!(
        matches!(&said, PortsError::Taken { host_port: 8080, owner: PortOwner::Program(Some(p)) } if p == "nginx"),
        "{said:?}"
    );
    let unknown = bind_failure(8080, &err, &Names(None));
    assert!(matches!(
        unknown,
        PortsError::Taken {
            owner: PortOwner::Program(None),
            ..
        }
    ));
}

#[test]
fn a_bind_the_system_refuses_says_why_in_words_a_user_can_act_on() {
    let denied = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
    let said = bind_failure(80, &denied, &Names(None));
    assert!(
        matches!(&said, PortsError::Cannot { host_port: 80, reason } if reason.contains("ports below 1024")),
        "{said:?}"
    );
    let other = std::io::Error::other("no buffer space");
    let said = bind_failure(9000, &other, &Names(None));
    assert!(
        matches!(&said, PortsError::Cannot { host_port: 9000, reason } if reason == "no buffer space"),
        "{said:?}"
    );
    assert!(
        said.to_string()
            .contains("cannot be opened: no buffer space")
    );
}
