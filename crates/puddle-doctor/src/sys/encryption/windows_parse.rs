// SPDX-License-Identifier: GPL-3.0-or-later
//! Reading the one line the `PowerShell` script prints (`windows.rs`), kept apart so every shape
//! of answer is tested on any OS.

use crate::facts::{Protection, SwapArea, SwapFacts};

/// The drive letter of a Windows path (`C:\x`, `\\?\C:\x`), upper case; `None` for a UNC path or
/// a relative one.
pub(super) fn drive_letter(path: &str) -> Option<char> {
    let path = path.strip_prefix(r"\\?\").unwrap_or(path);
    let mut chars = path.chars();
    let letter = chars.next().filter(char::is_ascii_alphabetic)?;
    (chars.next() == Some(':')).then(|| letter.to_ascii_uppercase())
}

/// What the shell's `System.Volume.BitLockerProtection` value (`x` when it read nothing) means,
/// as measured on a Windows Server 2025 runner: 1 protection on (also while encrypting), 2 off
/// (never encrypted, or decrypting), 3 and 4 a paused encryption or decryption, 5 suspended
/// protection, 6 locked, and 0 for every drive when the caller is not elevated (a standard user
/// and an administrator without elevation both get it, encrypted drive or not).
fn bitlocker(value: &str) -> Protection {
    match value {
        "1" | "6" => Protection::Encrypted {
            by: "BitLocker".into(),
        },
        "2" => Protection::NotEncrypted,
        "0" => unknown(
            "Windows gives the BitLocker state only to an elevated (administrator) process, \
             and answered 0 for the drive",
        ),
        "3" | "4" => unknown("BitLocker encryption or decryption of the drive is paused"),
        "5" => unknown(
            "BitLocker protection is suspended: the drive is encrypted but its key is stored in \
             the clear until protection is resumed",
        ),
        "x" => unknown("Windows gave no BitLocker state for the drive"),
        other => Protection::Unknown {
            why: format!("Windows gave BitLocker state {other}, which this check does not know"),
        },
    }
}

fn unknown(why: &str) -> Protection {
    Protection::Unknown { why: why.into() }
}

/// The parsed line: `puddle-encryption data=<v> pagefile-encrypted=<0|1> pagefiles=<D>:<v>,...`.
pub(super) struct Answer {
    pub(super) data: Protection,
    pub(super) swap: SwapFacts,
}

pub(super) fn parse(line: &str) -> Option<Answer> {
    let rest = line.trim().strip_prefix("puddle-encryption")?;
    let mut data = None;
    let mut efs = false;
    let mut files: Option<&str> = None;
    for field in rest.split_whitespace() {
        let (key, value) = field.split_once('=')?;
        match key {
            "data" => data = Some(bitlocker(value)),
            "pagefile-encrypted" => efs = value == "1",
            "pagefiles" => files = Some(value),
            _ => {}
        }
    }
    let areas: Vec<SwapArea> = files?
        .split(',')
        .filter(|f| !f.is_empty())
        .filter_map(|f| f.split_once(':'))
        .map(|(letter, value)| SwapArea {
            name: format!("{letter}:\\pagefile.sys"),
            protection: match bitlocker(value) {
                Protection::Encrypted { by } => Protection::Encrypted { by },
                _ if efs => Protection::Encrypted {
                    by: "encrypted page file setting".into(),
                },
                other => other,
            },
        })
        .collect();
    Some(Answer {
        data: data?,
        swap: if areas.is_empty() {
            SwapFacts::None
        } else {
            SwapFacts::Areas(areas)
        },
    })
}

#[cfg(test)]
mod tests {
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
        let a =
            parse("puddle-encryption extra=1 data=1 pagefile-encrypted=0 pagefiles=C:1").unwrap();
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
        let Protection::Unknown { why } =
            parse("puddle-encryption data=5 pagefile-encrypted=0 pagefiles=")
                .unwrap()
                .data
        else {
            panic!("suspended protection must be unknown");
        };
        assert!(why.contains("suspended"), "{why}");
    }

    #[test]
    fn a_zero_from_an_unelevated_caller_is_not_called_unencrypted() {
        let Protection::Unknown { why } =
            parse("puddle-encryption data=0 pagefile-encrypted=0 pagefiles=D:0")
                .unwrap()
                .data
        else {
            panic!("0 must be unknown");
        };
        assert!(why.contains("elevated"), "{why}");
    }

    #[test]
    fn other_output_is_not_an_answer() {
        assert!(parse("").is_none());
        assert!(parse("Cannot be loaded because running scripts is disabled").is_none());
        assert!(parse("puddle-encryption pagefile-encrypted=0 pagefiles=").is_none());
    }
}
