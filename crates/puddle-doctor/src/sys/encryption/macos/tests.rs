// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn filevault_answers() {
    assert!(matches!(
        filevault("FileVault is On.\n"),
        Protection::Encrypted { .. }
    ));
    assert_eq!(filevault("FileVault is Off."), Protection::NotEncrypted);
    assert!(matches!(filevault("Error: no"), Protection::Unknown { .. }));
}

#[test]
fn swap_answers() {
    let on = "total = 1024.00M  used = 12.00M  free = 1012.00M  (encrypted)";
    assert!(matches!(swap_protection(on), Protection::Encrypted { .. }));
    let off = "total = 1024.00M  used = 12.00M  free = 1012.00M";
    assert_eq!(swap_protection(off), Protection::NotEncrypted);
    assert!(matches!(swap_protection(""), Protection::Unknown { .. }));
}
