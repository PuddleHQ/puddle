// SPDX-License-Identifier: AGPL-3.0-or-later
//! What the fake tools wrote, for the tests that read it back.

use std::path::Path;

use crate::common::Fakes;

impl Fakes {
    /// The log the fake tool installed at `path` wrote, empty when it never ran.
    pub(crate) fn log(path: &Path) -> String {
        std::fs::read_to_string(format!("{}.log", path.display())).unwrap_or_default()
    }
}
