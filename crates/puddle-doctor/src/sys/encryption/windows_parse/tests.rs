// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn drive_letters() {
    assert_eq!(drive_letter(r"c:\Users\dev"), Some('C'));
    assert_eq!(drive_letter(r"\\?\D:\data"), Some('D'));
    assert_eq!(drive_letter(r"\\server\share"), None);
    assert_eq!(drive_letter("relative"), None);
    assert_eq!(drive_letter(""), None);
}

#[test]
fn a_protected_drive_with_its_page_file_on_it() {
    let a = parse("puddle-encryption extra=1 data=1 pagefile-encrypted=0 pagefiles=C:1").unwrap();
    assert_eq!(
        a.data,
        Protection::Encrypted {
            by: "BitLocker".into()
        }
    );
    let areas = a.swap.areas();
    assert_eq!(areas[0].name, r"C:\pagefile.sys");
    assert!(matches!(areas[0].protection, Protection::Encrypted { .. }));
}

#[test]
fn an_encrypted_page_file_setting_covers_an_unprotected_drive() {
    let a = parse("puddle-encryption data=2 pagefile-encrypted=1 pagefiles=D:2").unwrap();
    assert_eq!(a.data, Protection::NotEncrypted);
    let areas = a.swap.areas();
    assert!(
        matches!(&areas[0].protection, Protection::Encrypted { by } if by.contains("page file"))
    );
}

#[test]
fn no_page_file_and_odd_values() {
    let a = parse("puddle-encryption data=x pagefile-encrypted=0 pagefiles=").unwrap();
    assert!(matches!(a.data, Protection::Unknown { .. }));
    assert_eq!(a.swap, SwapFacts::None);
    let b = parse("puddle-encryption data=9 pagefile-encrypted=0 pagefiles=C:2,D:x").unwrap();
    assert!(matches!(b.data, Protection::Unknown { .. }));
    let areas = b.swap.areas();
    assert_eq!(areas[0].protection, Protection::NotEncrypted);
    assert!(matches!(areas[1].protection, Protection::Unknown { .. }));
}

/// Values read from a real Windows Server 2025 volume in each `BitLocker` state.
#[test]
fn each_measured_state_means_what_it_showed() {
    let state = |v: &str| {
        parse(&format!(
            "puddle-encryption data={v} pagefile-encrypted=0 pagefiles="
        ))
        .unwrap()
        .data
    };
    for on in ["1", "6"] {
        assert!(matches!(state(on), Protection::Encrypted { .. }), "{on}");
    }
    assert_eq!(state("2"), Protection::NotEncrypted);
    for told in ["0", "3", "4", "5"] {
        assert!(matches!(state(told), Protection::Unknown { .. }), "{told}");
    }
}

#[test]
fn a_suspended_drive_is_not_called_encrypted_and_says_why() {
    let data = parse("puddle-encryption data=5 pagefile-encrypted=0 pagefiles=")
        .unwrap()
        .data;
    assert!(
        matches!(&data, Protection::Unknown { why } if why.contains("suspended")),
        "{data:?}"
    );
}

#[test]
fn a_zero_from_an_unelevated_caller_is_not_called_unencrypted() {
    let data = parse("puddle-encryption data=0 pagefile-encrypted=0 pagefiles=D:0")
        .unwrap()
        .data;
    assert!(
        matches!(&data, Protection::Unknown { why } if why.contains("elevated")),
        "{data:?}"
    );
}

#[test]
fn other_output_is_not_an_answer() {
    assert!(parse("").is_none());
    assert!(parse("Cannot be loaded because running scripts is disabled").is_none());
    assert!(parse("puddle-encryption pagefile-encrypted=0 pagefiles=").is_none());
}
