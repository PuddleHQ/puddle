// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;

#[test]
fn a_host_with_no_windows_stores_has_no_corporate_roots() {
    let roots = SystemPlatform::new().corporate_roots().unwrap();
    assert!(roots.is_empty());
}
