// SPDX-License-Identifier: AGPL-3.0-or-later
//! Windows: the shell's `System.Volume.BitLockerProtection` property (the one Explorer shows),
//! which answers 0 ("unknown") unless the process is elevated, so a normal doctor run reports the
//! drive as unknown; the `BitLocker` WMI classes and `manage-bde` need elevation too, and no other
//! source was found that does not. Asked of `PowerShell`, because the COM calls behind it would be unsafe code that cannot be tried on a
//! machine without `BitLocker` here. No unsafe code.

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use super::windows_parse::{drive_letter, parse};
use crate::facts::{DataEncryption, DiskEncryptionFacts, ProcessOutcome, Protection, SwapFacts};
use crate::launch;

/// `PowerShell` takes about a second to start; a machine that needs more than this is not asked.
const LIMIT: Duration = Duration::from_secs(8);

/// `CREATE_NO_WINDOW`: no console flashes up when a GUI host runs the check.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Prints one line: the data drive's state, whether page files are encrypted by the file system
/// setting, and each page file's drive with its state (`x` when the shell gave none).
const SCRIPT: &str = r"
$ErrorActionPreference = 'Stop'
$shell = New-Object -ComObject Shell.Application
function State($letter) {
  $v = $shell.NameSpace($letter + ':').Self.ExtendedProperty('System.Volume.BitLockerProtection')
  if ($null -eq $v) { 'x' } else { [string]$v }
}
$mm = Get-ItemProperty 'HKLM:\SYSTEM\CurrentControlSet\Control\Session Manager\Memory Management'
$enc = Get-ItemProperty 'HKLM:\SYSTEM\CurrentControlSet\Control\FileSystem' -Name NtfsEncryptPagingFile -ErrorAction SilentlyContinue
$efs = if ($enc -and $enc.NtfsEncryptPagingFile -eq 1) { 1 } else { 0 }
$files = @()
foreach ($entry in @($mm.PagingFiles)) {
  if ($entry -match '^(\?|[A-Za-z]):') {
    $l = if ($Matches[1] -eq '?') { $env:SystemDrive.Substring(0, 1) } else { $Matches[1] }
    $files += ($l.ToUpper() + ':' + (State $l))
  }
}
'puddle-encryption data=' + (State $args[0]) + ' pagefile-encrypted=' + $efs + ' pagefiles=' + ($files -join ',')
";

pub(crate) fn disk_encryption(dir: Option<&Path>) -> DiskEncryptionFacts {
    let Some(dir) = dir else {
        return DiskEncryptionFacts {
            data: DataEncryption::NoFolder,
            swap: SwapFacts::Unreadable {
                why: "Puddle's data folder could not be found, so no drive was asked".into(),
            },
        };
    };
    let unknown = |why: String| DiskEncryptionFacts {
        data: DataEncryption::Volume {
            dir: dir.to_path_buf(),
            protection: Protection::Unknown { why: why.clone() },
        },
        swap: SwapFacts::Unreadable { why },
    };
    let Some(letter) = drive_letter(&dir.display().to_string()) else {
        return unknown(format!(
            "{} is not on a lettered drive, so its BitLocker state was not asked",
            dir.display()
        ));
    };
    match run(letter) {
        Ok(line) => match parse(&line) {
            Some(answer) => DiskEncryptionFacts {
                data: DataEncryption::Volume {
                    dir: dir.to_path_buf(),
                    protection: answer.data,
                },
                swap: answer.swap,
            },
            None => unknown(format!("PowerShell answered: {}", line.trim())),
        },
        Err(why) => unknown(why),
    }
}

fn powershell() -> PathBuf {
    let root =
        std::env::var_os("SystemRoot").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
    root.join(r"System32\WindowsPowerShell\v1.0\powershell.exe")
}

fn run(letter: char) -> Result<String, String> {
    let mut command = Command::new(powershell());
    command
        .args(["-NoProfile", "-NonInteractive", "-Command"])
        .arg(format!("& {{ {SCRIPT} }} {letter}"))
        .creation_flags(CREATE_NO_WINDOW);
    match launch::run(&mut command, LIMIT) {
        ProcessOutcome::Exited {
            code: Some(0),
            stdout_first_line,
            ..
        } => Ok(stdout_first_line),
        ProcessOutcome::Exited {
            code, stderr_tail, ..
        } => Err(format!(
            "PowerShell exited with {code:?}: {}",
            stderr_tail.lines().next().unwrap_or("no error text")
        )),
        ProcessOutcome::SpawnFailed { detail, .. } => {
            Err(format!("PowerShell did not start: {detail}"))
        }
        ProcessOutcome::TimedOut { after } => {
            Err(format!("PowerShell did not answer within {after:?}"))
        }
    }
}
