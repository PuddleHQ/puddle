// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn route(host: &str) -> Result<(String, Option<u16>), Refusal> {
    route_of(Some(host), 4000).map(|r| (r.workspace.to_string(), r.guest_port))
}

#[test]
fn a_workspace_address_names_the_workspace_in_any_case() {
    for host in ["acme.localhost:4000", "ACME.Localhost:4000"] {
        assert_eq!(route(host), Ok(("acme".to_owned(), None)));
    }
}

#[test]
fn a_port_label_in_front_names_the_forwarded_guest_port() {
    for (host, port) in [
        ("5173.acme.localhost:4000", 5173),
        ("1.ACME.localhost:4000", 1),
        ("65535.acme.localhost:4000", 65535),
    ] {
        assert_eq!(route(host), Ok(("acme".to_owned(), Some(port))), "{host}");
    }
    // A workspace may be named by digits: that is its editor, not a port.
    assert_eq!(route("5173.localhost:4000"), Ok(("5173".to_owned(), None)));
}

#[test]
fn every_other_address_is_refused() {
    let foreign = [
        "acme.localhost",
        "acme.localhost:4001",
        "acme.localhost:04000",
        "acme.localhost:+4000",
        "acme.localhost.:4000",
        "acme.localhost.evil.test:4000",
        "acme.sub.localhost:4000",
        "localhost:4000",
        "127.0.0.1:4000",
        "[::1]:4000",
        "evil.test:4000",
        "acme.localhost@evil.test:4000",
        "",
        "0.acme.localhost:4000",
        "05173.acme.localhost:4000",
        "65536.acme.localhost:4000",
        "+80.acme.localhost:4000",
        "80x.acme.localhost:4000",
        "80.acme.sub.localhost:4000",
        "80.81.acme.localhost:4000",
        "80.acme.localhost:4001",
        "80.acme.localhost",
    ];
    for host in foreign {
        assert_eq!(route(host), Err(Refusal::Foreign), "{host:?}");
    }
    assert_eq!(route_of(None, 4000), Err(Refusal::Missing));
}

#[test]
fn the_desktop_shells_own_names_and_bad_labels_are_not_workspaces() {
    for host in [
        "tauri.localhost:4000",
        "ipc.localhost:4000",
        "asset.localhost:4000",
        "-a.localhost:4000",
        "a_b.localhost:4000",
        ".localhost:4000",
        "80.tauri.localhost:4000",
        "80..localhost:4000",
    ] {
        assert_eq!(route(host), Err(Refusal::NotAWorkspace), "{host:?}");
    }
}

#[test]
fn an_origin_is_the_host_in_lower_case() {
    assert_eq!(
        origin_of("Acme.localhost:4000"),
        "http://acme.localhost:4000"
    );
}
