// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn version_matches_manifest() {
    assert_eq!(VERSION, "0.0.0");
}

#[test]
fn version_line_names_program_then_version() {
    assert_eq!(version_line("puddle-agent"), "puddle-agent 0.0.0");
}
