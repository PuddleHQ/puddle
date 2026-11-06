// SPDX-License-Identifier: GPL-3.0-or-later
//! macOS: `fdesetup status` (no administrator rights needed) for the boot volume that holds the
//! data folder, and `sysctl vm.swapusage`, which ends in `(encrypted)` when swap is.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use crate::facts::{
    DataEncryption, DiskEncryptionFacts, ProcessOutcome, Protection, SwapArea, SwapFacts,
};
use crate::launch;

const LIMIT: Duration = Duration::from_secs(5);

pub(crate) fn disk_encryption(dir: Option<&Path>) -> DiskEncryptionFacts {
    let vault = run("/usr/bin/fdesetup", &["status"]).map(|t| filevault(&t));
    let swap = run("/usr/sbin/sysctl", &["-n", "vm.swapusage"]);
    DiskEncryptionFacts {
        data: dir.map_or(DataEncryption::NoFolder, |dir| DataEncryption::Volume {
            dir: dir.to_path_buf(),
            protection: vault.unwrap_or_else(|why| Protection::Unknown { why }),
        }),
        swap: match swap {
            Ok(text) => SwapFacts::Areas(vec![SwapArea {
                name: "macOS swap".into(),
                protection: swap_protection(&text),
            }]),
            Err(why) => SwapFacts::Unreadable { why },
        },
    }
}

fn run(program: &str, args: &[&str]) -> Result<String, String> {
    match launch::run(Command::new(program).args(args), LIMIT) {
        ProcessOutcome::Exited {
            code: Some(0),
            stdout_first_line,
            ..
        } => Ok(stdout_first_line),
        ProcessOutcome::Exited { code, .. } => Err(format!("{program} exited with {code:?}")),
        ProcessOutcome::SpawnFailed { detail, .. } => {
            Err(format!("{program} did not start: {detail}"))
        }
        ProcessOutcome::TimedOut { after } => {
            Err(format!("{program} did not answer within {after:?}"))
        }
    }
}

fn filevault(text: &str) -> Protection {
    let text = text.trim();
    if text.starts_with("FileVault is On") {
        Protection::Encrypted {
            by: "FileVault".into(),
        }
    } else if text.starts_with("FileVault is Off") {
        Protection::NotEncrypted
    } else {
        Protection::Unknown {
            why: format!("fdesetup said: {text}"),
        }
    }
}

fn swap_protection(text: &str) -> Protection {
    if text.trim_end().ends_with("(encrypted)") {
        Protection::Encrypted {
            by: "macOS encrypted swap".into(),
        }
    } else if text.contains("total =") {
        Protection::NotEncrypted
    } else {
        Protection::Unknown {
            why: format!("sysctl said: {}", text.trim()),
        }
    }
}

#[cfg(test)]
mod tests {
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
}
