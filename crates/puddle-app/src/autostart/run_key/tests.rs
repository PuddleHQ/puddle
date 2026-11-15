// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn the_real_run_key_is_named_where_windows_reads_it() {
    // Only the name is checked: nothing here reads or writes the user's own list.
    let real = RunKey::user();
    assert_eq!(
        real.subkey,
        r"Software\Microsoft\Windows\CurrentVersion\Run"
    );
    assert_eq!(real.name, "Puddle");
}
