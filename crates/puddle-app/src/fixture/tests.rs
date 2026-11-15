// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn the_fixture_has_the_terminals_its_variable_names_and_none_otherwise() {
    assert_eq!(terminals_from(None), 0);
    assert_eq!(terminals_from(Some("")), 0);
    assert_eq!(terminals_from(Some("many")), 0);
    assert_eq!(terminals_from(Some(" 2 ")), 2);
}
