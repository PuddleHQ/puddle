// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn every_owner_reads_as_a_phrase_that_fits_after_in_use_by() {
    let ws = WorkspaceId::new("shop").unwrap();
    assert_eq!(
        PortOwner::Forward {
            workspace: ws.clone(),
            guest_port: 8080
        }
        .to_string(),
        "workspace shop's forward of its port 8080"
    );
    assert_eq!(
        PortOwner::Pinned {
            workspace: ws,
            guest_port: 3000
        }
        .to_string(),
        "workspace shop's pinned forward of its port 3000"
    );
    assert_eq!(
        PortOwner::Puddle(EndpointKind::Api).to_string(),
        "Puddle's own API"
    );
    assert_eq!(
        PortOwner::Program(Some("node".into())).to_string(),
        "another program (node)"
    );
    assert_eq!(
        PortOwner::Program(None).to_string(),
        "another program on this computer"
    );
}

#[test]
fn the_system_names_a_program_that_listens_and_has_nothing_to_say_about_a_free_port() {
    let port = puddle_test_kit::quiet_port();
    let listener = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
    let name = SystemPrograms.program(port);
    assert!(
        name.as_deref().is_some_and(|n| !n.is_empty()),
        "this process listens on {port}: {name:?}"
    );
    drop(listener);
    assert_eq!(SystemPrograms.program(port), None);
    assert_eq!(SystemPrograms.program(0), None);
}
