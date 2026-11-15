// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn outside_windows_there_is_no_job() {
    assert_eq!(contain_this_process().unwrap(), Containment::NotNeeded);
}
