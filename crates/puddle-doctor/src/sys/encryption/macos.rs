// SPDX-License-Identifier: AGPL-3.0-or-later
//! macOS: `fdesetup status` (no administrator rights needed) for the boot volume that holds the
//! data folder, and `sysctl vm.swapusage`, which ends in `(encrypted)` when swap is.

use std::path::Path;
use std::time::Duration;

use super::ProbeError;
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
            protection: vault.unwrap_or_else(|why| Protection::Unknown {
                why: why.to_string(),
            }),
        }),
        swap: match swap {
            Ok(text) => SwapFacts::Areas(vec![SwapArea {
                name: "macOS swap".into(),
                protection: swap_protection(&text),
            }]),
            Err(why) => SwapFacts::Unreadable {
                why: why.to_string(),
            },
        },
    }
}

fn run(program: &str, args: &[&str]) -> Result<String, ProbeError> {
    match launch::run(puddle_process::command(program).args(args), LIMIT) {
        ProcessOutcome::Exited {
            code: Some(0),
            stdout_first_line,
            ..
        } => Ok(stdout_first_line),
        ProcessOutcome::Exited { code, .. } => Err(ProbeError::Exited {
            program: program.to_owned(),
            code,
            stderr: None,
        }),
        ProcessOutcome::SpawnFailed { detail, .. } => Err(ProbeError::NotStarted {
            program: program.to_owned(),
            detail,
        }),
        ProcessOutcome::TimedOut { after } => Err(ProbeError::TimedOut {
            program: program.to_owned(),
            after,
        }),
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
mod tests;
