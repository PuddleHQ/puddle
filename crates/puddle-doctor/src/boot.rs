// SPDX-License-Identifier: AGPL-3.0-or-later
//! The test boot: msb boots a microVM from a tiny root file system that holds one program, which
//! exits with [`PROBE_EXIT_CODE`]. That exit code proves the whole path: msb started, the
//! hypervisor made a VM, the guest kernel and msb's agent came up, and a program ran inside.
//!
//! It needs no image download and no network. msb runs with a throwaway `MSB_HOME`, so the test
//! leaves nothing in Puddle's real one. The runtime logs at `debug`; when a boot fails and a keep
//! folder is given, its `logs/` folder (runtime, kernel and exec logs) is copied there first.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use puddle_fs::private;
use puddle_runtime::{GuestArch, RuntimeEnv, RuntimeLayout};

use crate::diagnose::PROBE_EXIT_CODE;
use crate::facts::{BootFacts, KeptLogs, ProcessOutcome};
use crate::launch;

/// The probe's path inside the guest.
const PROBE_GUEST_PATH: &str = "/probe";

/// The test VM's sandbox name. Named, so msb keeps the sandbox (and its logs) after the probe
/// exits instead of removing it as a one-off run; the throwaway home goes with it.
const BOOT_SANDBOX: &str = "puddle-doctor-boot";

/// Log level of the test VM's runtime (`runtime.log`): the VMM trace a failed boot needs.
const BOOT_LOG_LEVEL: &str = "debug";

/// The folder inside Puddle's data folder where failed test boots' logs are kept.
const BOOT_LOGS_FOLDER: &str = "doctor-boot-logs";

/// How many failed boots' logs stay in that folder; keeping one more removes the oldest.
pub const KEPT_BOOT_LOGS: usize = 3;

/// Where the failed test boots of the installation whose data folder is `data` keep their logs.
#[must_use]
pub fn boot_logs_dir(data: &Path) -> PathBuf {
    data.join(BOOT_LOGS_FOLDER)
}

/// A statically linked x86-64 Linux program of 132 bytes, without libc or loader: one ELF header,
/// one loadable segment, and the code `mov edi, 42; mov eax, 231 (exit_group); syscall`.
#[must_use]
pub fn probe_x86_64() -> Vec<u8> {
    const BASE: u64 = 0x40_0000;
    const EHDR: u16 = 64;
    const PHDR: u16 = 56;
    let exit = u8::try_from(PROBE_EXIT_CODE).unwrap_or(42);
    let code = [
        0xbf, exit, 0, 0, 0, // mov edi, 42
        0xb8, 0xe7, 0, 0, 0, // mov eax, 231 (exit_group)
        0x0f, 0x05, // syscall
    ];
    let total = u64::from(EHDR) + u64::from(PHDR) + code.len() as u64;
    let mut elf = Vec::with_capacity(132);
    // e_ident: magic, 64-bit, little-endian, version 1, System V ABI, padding.
    elf.extend_from_slice(&[0x7f, b'E', b'L', b'F', 2, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    elf.extend_from_slice(&2_u16.to_le_bytes()); // e_type: executable
    elf.extend_from_slice(&0x3e_u16.to_le_bytes()); // e_machine: x86-64
    elf.extend_from_slice(&1_u32.to_le_bytes()); // e_version
    elf.extend_from_slice(&(BASE + u64::from(EHDR + PHDR)).to_le_bytes()); // e_entry
    elf.extend_from_slice(&u64::from(EHDR).to_le_bytes()); // e_phoff
    elf.extend_from_slice(&0_u64.to_le_bytes()); // e_shoff: no sections
    elf.extend_from_slice(&0_u32.to_le_bytes()); // e_flags
    elf.extend_from_slice(&EHDR.to_le_bytes()); // e_ehsize
    elf.extend_from_slice(&PHDR.to_le_bytes()); // e_phentsize
    elf.extend_from_slice(&1_u16.to_le_bytes()); // e_phnum
    elf.extend_from_slice(&64_u16.to_le_bytes()); // e_shentsize
    elf.extend_from_slice(&0_u16.to_le_bytes()); // e_shnum
    elf.extend_from_slice(&0_u16.to_le_bytes()); // e_shstrndx
    elf.extend_from_slice(&1_u32.to_le_bytes()); // p_type: PT_LOAD
    elf.extend_from_slice(&5_u32.to_le_bytes()); // p_flags: read + execute
    elf.extend_from_slice(&0_u64.to_le_bytes()); // p_offset
    elf.extend_from_slice(&BASE.to_le_bytes()); // p_vaddr
    elf.extend_from_slice(&BASE.to_le_bytes()); // p_paddr
    elf.extend_from_slice(&total.to_le_bytes()); // p_filesz
    elf.extend_from_slice(&total.to_le_bytes()); // p_memsz
    elf.extend_from_slice(&0x1000_u64.to_le_bytes()); // p_align
    elf.extend_from_slice(&code);
    elf
}

/// The probe program for guests of `arch`, or `None` while none is written for it. Only
/// [`GuestArch::X86_64`] has one (it is the only guest Puddle builds, [`GuestArch::is_built`]); an
/// `aarch64` probe goes here when that guest does.
#[must_use]
pub fn probe_for(arch: GuestArch) -> Option<Vec<u8>> {
    match arch {
        GuestArch::X86_64 => Some(probe_x86_64()),
        GuestArch::Aarch64 => None,
    }
}

/// Writes the guest's root file system into `dir`: the `probe` program, plus the `/etc/passwd` and
/// `/etc/group` msb's agent reads to resolve the user it runs a command as.
///
/// # Errors
///
/// Any file system error.
pub fn write_rootfs(dir: &Path, probe_program: &[u8]) -> std::io::Result<()> {
    let probe = dir.join(PROBE_GUEST_PATH.trim_start_matches('/'));
    std::fs::write(&probe, probe_program)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&probe, std::fs::Permissions::from_mode(0o755))?;
    }
    let etc = dir.join("etc");
    std::fs::create_dir_all(&etc)?;
    std::fs::write(
        etc.join("passwd"),
        format!("root:x:0:0:root:/:{PROBE_GUEST_PATH}\n"),
    )?;
    std::fs::write(etc.join("group"), "root:x:0:\n")?;
    Ok(())
}

/// Boots a test VM with the msb in `runtime_dir`, giving up after `limit`. Uses a throwaway msb
/// home and root file system in the temp dir. If the probe doesn't exit with
/// [`PROBE_EXIT_CODE`] and `keep_logs` is set, msb's logs of the boot are copied to
/// `<keep_logs>/puddle-doctor-boot-<ms since 1970>/logs/` before the home is deleted; the
/// result's `kept_logs` says where they went, or why they didn't.
#[must_use]
pub fn test_boot(
    runtime_dir: &Path,
    arch: &str,
    limit: Duration,
    keep_logs: Option<&Path>,
) -> BootFacts {
    let Some(probe) = GuestArch::parse(arch).and_then(probe_for) else {
        return BootFacts::UnsupportedArch {
            arch: arch.to_owned(),
        };
    };
    let start = Instant::now();
    let work = match work_dir() {
        Ok(dir) => dir,
        Err(e) => {
            return BootFacts::SetupFailed {
                detail: format!("temp dir: {e}"),
            };
        }
    };
    let rootfs = work.path().join("rootfs");
    let prepared = std::fs::create_dir_all(&rootfs).and_then(|()| write_rootfs(&rootfs, &probe));
    if let Err(e) = prepared {
        return BootFacts::SetupFailed {
            detail: format!("{}: {e}", rootfs.display()),
        };
    }
    let layout = match RuntimeLayout::new(runtime_dir.to_path_buf(), work.path().join("home")) {
        Ok(layout) => layout,
        Err(e) => {
            return BootFacts::SetupFailed {
                detail: e.to_string(),
            };
        }
    };
    let outcome = launch::run(
        &mut command(&layout, &rootfs),
        limit.saturating_sub(start.elapsed()),
    );
    let booted = matches!(
        outcome,
        ProcessOutcome::Exited {
            code: Some(PROBE_EXIT_CODE),
            ..
        }
    );
    let kept_logs = keep_logs
        .filter(|_| !booted)
        .map(|dir| keep_boot_logs(layout.home(), dir));
    BootFacts::Ran { outcome, kept_logs }
}

/// The throwaway folder that holds the test boot's msb home and root file system. It is
/// owner-only whatever the umask: msb refuses a catalog with an ancestor that other accounts
/// can write to, and a umask of `0002` would otherwise leave this folder group-writable.
fn work_dir() -> std::io::Result<tempfile::TempDir> {
    let mut builder = tempfile::Builder::new();
    builder.prefix("puddle-doctor-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(std::fs::Permissions::from_mode(0o700));
    }
    builder.tempdir()
}

/// Copies the test sandbox's log files from msb `home` to a new folder in `dir`. Keeping is best
/// effort and never changes the boot's result, but the answer says where the logs went or why
/// they didn't, because the caller asked for them.
fn keep_boot_logs(home: &Path, dir: &Path) -> KeptLogs {
    let from = home.join("sandboxes").join(BOOT_SANDBOX).join("logs");
    let entries = match std::fs::read_dir(&from) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return KeptLogs::NotKept {
                detail: format!(
                    "msb wrote no logs for this boot ({} doesn't exist)",
                    from.display()
                ),
            };
        }
        Err(e) => {
            return KeptLogs::NotKept {
                detail: format!("{}: {e}", from.display()),
            };
        }
    };
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let kept = new_folder(dir, &format!("{BOOT_SANDBOX}-{stamp}")).and_then(|folder| {
        let to = folder.join("logs");
        std::fs::create_dir_all(&to).map(|()| (folder, to))
    });
    let (folder, to) = match kept {
        Ok(made) => made,
        Err(e) => {
            return KeptLogs::NotKept {
                detail: format!("no folder to keep them in under {}: {e}", dir.display()),
            };
        }
    };
    let failed = copy_logs(entries, &from, &to);
    prune_kept(dir, KEPT_BOOT_LOGS);
    KeptLogs::Kept {
        dir: folder,
        failed,
    }
}

/// Copies the files among `entries` (the listing of `from`) into `to`; the files and the listing
/// entries that couldn't be copied come back as one line each, with their error.
fn copy_logs(entries: std::fs::ReadDir, from: &Path, to: &Path) -> Vec<String> {
    entries
        .filter_map(|entry| {
            let copied = entry
                .map_err(|e| format!("{}: {e}", from.display()))
                .and_then(|entry| {
                    let path = entry.path();
                    if !path.is_file() {
                        return Ok(());
                    }
                    std::fs::copy(&path, to.join(entry.file_name()))
                        .map(drop)
                        .map_err(|e| format!("{}: {e}", path.display()))
                });
            copied.err()
        })
        .collect()
}

/// Deletes all but the newest `keep` kept-boot folders in `dir` (the name's timestamp orders
/// them). Best effort: a folder that can't be removed stays and the next failed boot tries
/// again, and anything in `dir` that isn't a kept boot is left alone.
fn prune_kept(dir: &Path, keep: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut kept: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(&format!("{BOOT_SANDBOX}-"))
                && e.file_type().is_ok_and(|t| t.is_dir())
        })
        .map(|e| e.path())
        .collect();
    kept.sort();
    let old = kept.len().saturating_sub(keep);
    for path in kept.into_iter().take(old) {
        drop(std::fs::remove_dir_all(path));
    }
}

/// Creates and returns a folder in `dir` named `name`, or `name-2`, `name-3`, ... when taken, so
/// two boots kept in the same millisecond don't share one. Both `dir` and the folder are
/// owner-only: a boot's logs name paths and machine details.
fn new_folder(dir: &Path, name: &str) -> std::io::Result<PathBuf> {
    private::create_dir(dir)?;
    for n in 1..=100 {
        let path = if n == 1 {
            dir.join(name)
        } else {
            dir.join(format!("{name}-{n}"))
        };
        match private::create_new_dir(&path) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        format!("{name} and its numbered variants all exist"),
    ))
}

fn command(layout: &RuntimeLayout, rootfs: &Path) -> Command {
    let mut cmd = Command::new(layout.msb_path());
    RuntimeEnv::plan(layout, std::env::vars_os()).apply_to(&mut cmd);
    cmd.arg("run").arg(rootfs).args([
        "--name",
        BOOT_SANDBOX,
        "--log-level",
        BOOT_LOG_LEVEL,
        "--no-stdin",
        "--",
        PROBE_GUEST_PATH,
    ]);
    cmd
}

#[cfg(test)]
mod tests;
