// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn every_entry_says_how_it_is_found_why_it_matters_and_what_to_do() {
    for entry in PROBLEMATIC_SOFTWARE {
        assert_ne!(entry.name, "");
        assert!(
            !entry.services.is_empty(),
            "{} has no detection",
            entry.name
        );
        assert_ne!(entry.why, "");
        assert_ne!(entry.remedy, "");
    }
}

#[test]
fn names_are_unique() {
    let mut names: Vec<_> = PROBLEMATIC_SOFTWARE.iter().map(|s| s.name).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), PROBLEMATIC_SOFTWARE.len());
}
