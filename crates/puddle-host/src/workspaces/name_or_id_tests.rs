// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn text_for_the_user_names_the_workspace_and_falls_back_to_the_id_only_when_it_is_gone() {
    let id = WorkspaceId::new("a0k3x9zq").unwrap();
    let name = DisplayName::parse("Acme shop").unwrap();
    assert_eq!(name_or_id(Some(&name), &id), "Acme shop");
    assert_eq!(name_or_id(None, &id), "a0k3x9zq");
}
