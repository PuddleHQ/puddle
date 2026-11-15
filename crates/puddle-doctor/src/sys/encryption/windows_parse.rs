// SPDX-License-Identifier: AGPL-3.0-or-later
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
mod tests;
