// SPDX-License-Identifier: AGPL-3.0-or-later
use proptest::prelude::*;

use super::*;

const BASE: &str = "puddle.example.com";

fn id(text: &str) -> WorkspaceId {
    WorkspaceId::new(text).expect("a valid id")
}

fn known(ids: &[&str]) -> BTreeSet<WorkspaceId> {
    ids.iter().map(|text| id(text)).collect()
}

fn read(host: &str, ids: &[&str]) -> Result<(String, Option<u16>), Refusal> {
    route_of_label(Some(host), BASE, &known(ids)).map(|r| (r.workspace.to_string(), r.guest_port))
}

fn pair(id_text: &str, name: &str) -> (WorkspaceId, String) {
    (id(id_text), name.to_owned())
}

#[test]
fn a_label_that_is_an_id_opens_the_editor_in_any_case() {
    for host in ["k3x9q7m2.puddle.example.com", "K3X9Q7M2.Puddle.Example.COM"] {
        assert_eq!(read(host, &["k3x9q7m2"]), Ok(("k3x9q7m2".to_owned(), None)));
    }
}

#[test]
fn a_port_then_hyphen_then_id_names_a_forwarded_port() {
    for (host, port) in [
        ("5173-k3x9q7m2.puddle.example.com", 5173),
        ("1-k3x9q7m2.puddle.example.com", 1),
        ("65535-k3x9q7m2.puddle.example.com", 65535),
    ] {
        assert_eq!(
            read(host, &["k3x9q7m2"]),
            Ok(("k3x9q7m2".to_owned(), Some(port))),
            "{host}"
        );
    }
}

#[test]
fn a_legacy_id_with_hyphens_is_read_whole_and_after_its_port() {
    let ids = ["acme-api"];
    assert_eq!(
        read("acme-api.puddle.example.com", &ids),
        Ok(("acme-api".to_owned(), None))
    );
    assert_eq!(
        read("8080-acme-api.puddle.example.com", &ids),
        Ok(("acme-api".to_owned(), Some(8080)))
    );
}

#[test]
fn bad_ports_unknown_ids_and_other_hosts_are_refused() {
    let ids = ["k3x9q7m2"];
    let not_a_workspace = [
        "0-k3x9q7m2",
        "05173-k3x9q7m2",
        "65536-k3x9q7m2",
        "+80-k3x9q7m2",
        "-k3x9q7m2",
        "80-",
        "80-unknown1",
        "unknown1",
        "80k3x9q7m2",
    ];
    for label in not_a_workspace {
        assert_eq!(
            read(&format!("{label}.{BASE}"), &ids),
            Err(Refusal::NotAWorkspace),
            "{label}"
        );
    }
    for host in [
        BASE,
        ".puddle.example.com",
        "a.k3x9q7m2.puddle.example.com",
        "5173.k3x9q7m2.puddle.example.com",
        "k3x9q7m2.puddle.example.org",
        "k3x9q7m2.localhost",
        "k3x9q7m2.xpuddle.example.com",
    ] {
        assert_eq!(read(host, &ids), Err(Refusal::Foreign), "{host}");
    }
    assert_eq!(
        route_of_label(None, BASE, &known(&ids)),
        Err(Refusal::Missing)
    );
}

#[test]
fn the_editor_wins_when_a_legacy_id_reads_as_a_port_of_another() {
    let ids = ["api", "8080-api"];
    assert_eq!(
        read("8080-api.puddle.example.com", &ids),
        Ok(("8080-api".to_owned(), None))
    );
    assert_eq!(
        read("api.puddle.example.com", &ids),
        Ok(("api".to_owned(), None))
    );
}

#[test]
fn two_legacy_ids_that_clash_raise_one_problem_naming_both_workspaces() {
    let workspaces = [pair("api", "Shop API"), pair("8080-api", "Eight")];
    let problems = shadow_problems(&workspaces);
    assert_eq!(problems.len(), 1);
    let problem = &problems[0];
    assert_eq!(problem.key, "host-label-shadow:8080-api");
    assert_eq!(problem.workspace, Some(id("8080-api")));
    assert!(problem.title.contains("Eight") && problem.title.contains("Shop API"));
    assert!(problem.detail.contains("/proxy/8080/"));
}

#[test]
fn ids_that_do_not_clash_raise_nothing() {
    let workspaces = [
        pair("api", "A"),
        pair("0-api", "B"),
        pair("8080-web", "C"),
        pair("k3x9q7m2", "D"),
    ];
    assert_eq!(shadows(&workspaces).len(), 0);
    assert_eq!(shadow_problems(&[]).len(), 0);
}

/// A generated id: eight characters of Crockford base32, a letter first.
fn generated_id() -> impl Strategy<Value = WorkspaceId> {
    any::<[u8; 5]>().prop_map(WorkspaceId::from_random)
}

proptest! {
    #[test]
    fn a_generated_id_is_never_shadowed(ids in proptest::collection::vec(generated_id(), 0..12)) {
        let workspaces: Vec<_> = ids.iter().cloned().map(|id| (id, String::new())).collect();
        prop_assert_eq!(shadows(&workspaces).len(), 0);
        let set: BTreeSet<_> = ids.into_iter().collect();
        for workspace in &set {
            let editor = route_of_label(Some(&format!("{workspace}.{BASE}")), BASE, &set)?;
            prop_assert_eq!(&editor.workspace, workspace);
            prop_assert_eq!(editor.guest_port, None);
        }
    }

    #[test]
    fn a_generated_id_reads_back_as_its_editor_and_as_any_valid_port(
        workspace in generated_id(),
        port in 1u16..=u16::MAX,
    ) {
        let set = BTreeSet::from([workspace.clone()]);
        let editor = route_of_label(Some(&format!("{workspace}.{BASE}")), BASE, &set)?;
        prop_assert_eq!((editor.workspace, editor.guest_port), (workspace.clone(), None));
        let forwarded = route_of_label(Some(&format!("{port}-{workspace}.{BASE}")), BASE, &set)?;
        prop_assert_eq!((forwarded.workspace, forwarded.guest_port), (workspace, Some(port)));
    }

    #[test]
    fn a_legacy_pair_shadows_exactly_when_the_port_is_valid(
        base_id in "[a-z][a-z0-9]{0,10}",
        port in 0u32..=70_000,
    ) {
        let legacy = format!("{port}-{base_id}");
        let workspaces = [pair(&base_id, "one"), pair(&legacy, "two")];
        let valid = (1..=65_535).contains(&port);
        let found = shadows(&workspaces);
        prop_assert_eq!(found.len(), usize::from(valid));
        if valid {
            prop_assert_eq!(found[0].guest_port, u16::try_from(port).unwrap());
            let set: BTreeSet<_> = workspaces.iter().map(|(id, _)| id.clone()).collect();
            let route = route_of_label(Some(&format!("{legacy}.{BASE}")), BASE, &set)?;
            prop_assert_eq!(route.guest_port, None);
        }
    }
}
