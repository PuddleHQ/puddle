// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn repo() -> store::RepoRef {
    store::RepoRef::new("github.com", "acme", "shop").unwrap()
}

#[test]
fn only_a_repository_nobody_covers_has_a_warning_naming_the_identity_and_the_place() {
    let start = |basis, label: Option<&str>| store::GitStart {
        identity: label.map(|l| (store::IdentityId(1), l.to_owned())),
        basis,
    };
    let covered =
        NewWorkspaceIdentity::from_store(&start(store::StartBasis::Covers, Some("Work")), &repo());
    assert_eq!(covered.warning, None);
    assert_eq!(covered.identity.as_deref(), Some("Work"));

    let default = NewWorkspaceIdentity::from_store(
        &start(store::StartBasis::Default, Some("Personal")),
        &repo(),
    );
    assert_eq!(default.basis, NewWorkspaceIdentityBasis::Default);
    let text = default.warning.unwrap();
    assert!(
        text.contains("Personal") && text.contains("github.com/acme"),
        "{text}"
    );

    let none =
        NewWorkspaceIdentity::from_store(&start(store::StartBasis::NoIdentity, None), &repo());
    assert_eq!(
        (none.identity, none.basis),
        (None, NewWorkspaceIdentityBasis::None)
    );
    assert!(none.warning.unwrap().contains("no default identity"));
}
