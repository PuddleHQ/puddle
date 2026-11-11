// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;

#[test]
fn outside_windows_every_process_is_the_worker() {
    assert_eq!(supervise().unwrap(), Role::Worker);
}
