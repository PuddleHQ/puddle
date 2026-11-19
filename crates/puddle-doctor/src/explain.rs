// SPDX-License-Identifier: AGPL-3.0-or-later
//! What each check means for the person, and what to do, in at most two short sentences each.
//! A screen shows these first and keeps the long fix (commands, where to look) for later; an ok
//! check says what it protects or makes possible, so every line carries a reason to be there.

use crate::facts::Os;
use crate::report::{Check, CheckId, Finding, Status};

/// Fills [`Check::impact`] and [`Check::remedy`] from the check's id, status and finding.
pub(crate) fn explain(os: Os, check: &mut Check) {
    let (impact, remedy) = match (check.status, check.finding) {
        (Status::Ok, _) => (ok_impact(check.id), None),
        (Status::Skipped, _) => (skipped_impact(check.id), Some(SKIPPED_REMEDY)),
        (_, finding) => {
            // Every check that is not ok names its finding; without one it gets the generic words.
            let (impact, remedy) = finding.map_or(UNWORDED, |f| finding_text(os, f));
            (impact, Some(remedy))
        }
    };
    impact.clone_into(&mut check.impact);
    check.remedy = remedy.map(str::to_owned);
}

const SKIPPED_REMEDY: &str = "Fix any problem above, then check again.";

/// What an ok check protects or makes possible.
fn ok_impact(id: CheckId) -> &'static str {
    match id {
        CheckId::Virtualization => {
            "Lets the hypervisor run each workspace as a small virtual machine, kept apart from this computer."
        }
        CheckId::Hypervisor => {
            "Lets Puddle start workspaces as virtual machines, walled off from this computer and from each other."
        }
        CheckId::CodeIntegrity => {
            "Windows' code integrity rules guard the system against untrusted code, and Puddle runs alongside them."
        }
        CheckId::Runtime => {
            "Puddle has the exact runtime it was built and tested with, so workspaces start as expected."
        }
        CheckId::Launch => {
            "Puddle's runtime may run here: no security software or policy stops it."
        }
        CheckId::TestBoot => {
            "Shows that a workspace can start here: a test machine booted and ran a program."
        }
        CheckId::JobObject => "Puddle may start its virtual machines as processes of their own.",
        CheckId::GlobalSecureAccess => {
            "Rules out a known reason for company sites to fail from a workspace: the Global Secure Access client."
        }
        CheckId::DiskSpace => {
            "Leaves room for workspaces to install packages and build without stopping on a full drive."
        }
        CheckId::DataPath => {
            "Only you can change the folders above Puddle's data, so its runtime agrees to use them."
        }
        CheckId::DiskEncryption => {
            "Someone who takes this disk can't read Puddle's data or the secrets it keeps."
        }
        CheckId::SshAgent => {
            "Programs in a workspace can log in to servers with your SSH keys, and the keys never leave this computer."
        }
        CheckId::AzCli => "Workspaces can sign in to Azure DevOps through your `az` login.",
        CheckId::CredentialStore => {
            "Puddle keeps your tokens and passwords in a protected store, so you don't type them again."
        }
    }
}

/// What it costs that a check was not run.
fn skipped_impact(id: CheckId) -> &'static str {
    match id {
        CheckId::Launch => {
            "Not checked, so Puddle can't tell yet whether its runtime may run here."
        }
        CheckId::TestBoot => {
            "Not checked, so Puddle can't tell yet whether a workspace can start here."
        }
        CheckId::DiskSpace => "Not checked, so Puddle can't warn you before the drive fills up.",
        CheckId::DataPath => {
            "Not checked, so Puddle can't tell yet whether its runtime accepts the data folder."
        }
        _ => "Not checked, so Puddle can't tell yet whether this works.",
    }
}

/// The impact and the remedy of a finding.
fn finding_text(os: Os, finding: Finding) -> (&'static str, &'static str) {
    use Finding as F;
    match finding {
        F::FirmwareVirtualizationOff
        | F::WhpNotEnabled
        | F::HypervisorNotRunning
        | F::NestedVirtualizationOff
        | F::RunsInVirtualMachine
        | F::VirtualizationUnknown
        | F::HypervisorQueryFailed
        | F::KvmMissing
        | F::KvmAccessDenied
        | F::UnsupportedOs => machine(finding),
        F::RuntimeMissing
        | F::RuntimeVersionMismatch
        | F::RuntimeDigestMismatch
        | F::RuntimeUnreadable
        | F::RuntimePermissions
        | F::RuntimeOverridden
        | F::BlockedByAppLocker
        | F::BlockedByAppControl
        | F::BlockedByAntivirus
        | F::ProcessCreationDenied
        | F::DllMissing
        | F::LaunchFailed
        | F::LaunchExited
        | F::LaunchTimedOut
        | F::BootFailed
        | F::BootTimedOut
        | F::BootBlockedByJob
        | F::BootSetupFailed
        | F::JobWithoutBreakaway
        | F::JobUnreadable
        | F::CodeIntegrityUnreadable => runtime(finding),
        F::GlobalSecureAccessInstalled
        | F::GlobalSecureAccessUnknown
        | F::DiskSpaceLow
        | F::DiskSpaceCritical
        | F::DiskSpaceUnreadable
        | F::DataPathWritable
        | F::DataPathBlocked
        | F::DataPathAcl
        | F::DataPathUnreadable
        | F::DiskNotEncrypted
        | F::SwapNotEncrypted
        | F::DiskEncryptionUnknown => storage(os, finding),
        F::SshAgentMissing
        | F::SshAgentLocked
        | F::SshAgentEmpty
        | F::SshAgentSlow
        | F::SshAgentUnusable
        | F::AzCliMissing
        | F::AzCliUnreadable
        | F::CredentialStoreMissing
        | F::CredentialStoreLocked
        | F::CredentialStoreNoDesktop
        | F::CredentialStoreVaultUnreadable
        | F::CredentialStoreRecordUnreadable
        | F::CredentialStoreKeyRejected
        | F::CredentialStoreKeyUnopenable
        | F::CredentialStoreKeyFile
        | F::CredentialStoreTpmLibrariesMissing => ssh_azure_and_store(os, finding),
    }
}

/// The words for a problem with no finding, and for a finding sent to a function that does not
/// word it; neither happens in a report (the tests hold every report and every finding to that).
const UNWORDED: (&str, &str) = (
    "Puddle found something here it has no words for yet.",
    "Copy the report and send it with a bug report.",
);

/// The CPU, the hypervisor and the operating system.
fn machine(finding: Finding) -> (&'static str, &'static str) {
    use Finding as F;
    match finding {
        F::FirmwareVirtualizationOff => (
            "Puddle can't start any workspace: the CPU's virtualization feature is turned off.",
            "Turn on CPU virtualization in the PC's UEFI/BIOS setup, then start the PC again.",
        ),
        F::WhpNotEnabled => (
            "Puddle can't start any workspace: the Windows Hypervisor Platform feature is off.",
            "Turn on Windows Hypervisor Platform in the optional Windows features, then restart Windows.",
        ),
        F::HypervisorNotRunning => (
            "Puddle can't start any workspace: the Windows hypervisor isn't running.",
            "Restart Windows. If it still doesn't run, run `bcdedit /set hypervisorlaunchtype auto` as administrator and restart.",
        ),
        F::NestedVirtualizationOff => (
            "Puddle can't start any workspace: this computer is a virtual machine without nested virtualization.",
            "Turn on nested virtualization for this virtual machine on its host, or use a physical computer.",
        ),
        F::RunsInVirtualMachine => (
            "This computer is a virtual machine, so workspaces need its host to pass CPU virtualization through.",
            "Turn on nested virtualization for this virtual machine on its host.",
        ),
        F::VirtualizationUnknown => (
            "Puddle couldn't read whether CPU virtualization is on.",
            "Fix the Hypervisor line first. If it stays, check that virtualization is on in the UEFI/BIOS setup.",
        ),
        F::HypervisorQueryFailed => (
            "Puddle can't tell whether it can start workspaces here.",
            "Restart the computer and check again. If it stays, copy the report and send it with a bug report.",
        ),
        F::KvmMissing => (
            "Puddle can't start any workspace: KVM isn't available.",
            "Load KVM for your CPU (`sudo modprobe kvm_intel` or `kvm_amd`).",
        ),
        F::KvmAccessDenied => (
            "Puddle can't start any workspace: your account may not use KVM.",
            "Add yourself to the kvm group (`sudo usermod -aG kvm \"$USER\"`), then log out and in again.",
        ),
        F::UnsupportedOs => (
            "Puddle can't start workspaces on this operating system yet.",
            "Run Puddle on Windows 10 or 11, or on Linux with KVM.",
        ),
        _ => UNWORDED,
    }
}

/// The runtime: its files, its start and the test boot.
fn runtime(finding: Finding) -> (&'static str, &'static str) {
    use Finding as F;
    match finding {
        F::RuntimeMissing => (
            "Puddle can't start any workspace: part of its runtime is missing.",
            "Reinstall Puddle.",
        ),
        F::RuntimeVersionMismatch => (
            "Puddle can't start any workspace: its runtime is not the version it was built for.",
            "Reinstall Puddle.",
        ),
        F::RuntimeDigestMismatch => (
            "Puddle won't start workspaces: a file of its runtime changed after it was installed.",
            "Repair or reinstall Puddle.",
        ),
        F::RuntimeUnreadable => (
            "Puddle can't start any workspace: its runtime is damaged.",
            "Reinstall Puddle.",
        ),
        F::RuntimePermissions => (
            "Puddle can't start any workspace: your account may not run its runtime.",
            "Reset the permissions of the runtime file, or reinstall Puddle.",
        ),
        F::RuntimeOverridden => (
            "This development build runs a runtime it wasn't tested with.",
            "Unset PUDDLE_DEV_ANY_RUNTIME to test the runtime Puddle ships with.",
        ),
        F::BlockedByAppLocker => (
            "Puddle can't start any workspace: a Windows policy forbids its runtime.",
            "Ask whoever manages this PC to allow Puddle's install folder in AppLocker.",
        ),
        F::BlockedByAppControl => (
            "Puddle can't start any workspace: App Control or Smart App Control blocks its runtime.",
            "Ask whoever manages this PC to allow Puddle. Smart App Control can only be turned off.",
        ),
        F::BlockedByAntivirus => (
            "Puddle can't start any workspace: antivirus stopped its runtime.",
            "Restore msb from the antivirus quarantine and exclude Puddle's runtime folder.",
        ),
        F::ProcessCreationDenied => (
            "Puddle can't start any workspace: security software stops its runtime.",
            "Ask whoever runs your endpoint security to allow Puddle's runtime and its virtual machines.",
        ),
        F::DllMissing => (
            "Puddle can't start any workspace: a Windows file its runtime needs is missing.",
            "Fix the Hypervisor line if it failed, then reinstall Puddle and run Windows Update.",
        ),
        F::LaunchFailed => (
            "Puddle can't start any workspace: its runtime does not start.",
            "Reinstall Puddle. If it stays, copy the report and send it with a bug report.",
        ),
        F::LaunchExited => (
            "Puddle can't start any workspace: its runtime stops right after it starts.",
            "Look for msb in your security software's log, or reinstall Puddle.",
        ),
        F::LaunchTimedOut => (
            "Puddle can't start workspaces yet: its runtime did not answer in time.",
            "Check again. Security software scanning a new program is the usual cause.",
        ),
        F::BootFailed => (
            "Puddle can't start any workspace: the test machine did not boot.",
            "Restart the computer and check again. If it stays, look for msb in your security software's log.",
        ),
        F::BootTimedOut => (
            "Workspaces may not start: the test machine did not finish in time.",
            "Check again. If it keeps timing out, look for msb in your security software's log.",
        ),
        F::BootBlockedByJob => (
            "Puddle can't start any workspace from here: the program that started it won't let it create virtual machines.",
            "Start Puddle from the Start menu or a normal terminal, not from a CI runner or an IDE.",
        ),
        F::BootSetupFailed => (
            "Puddle couldn't prepare the test machine, so it can't tell whether workspaces start.",
            "Make sure your temp folder exists, is writable and has free space.",
        ),
        F::JobWithoutBreakaway => (
            "The program that started Puddle limits the processes Puddle may create, which can stop workspaces.",
            "If workspaces fail to start, start Puddle from the Start menu or a normal terminal.",
        ),
        F::JobUnreadable => (
            "Puddle couldn't read whether the program that started it limits its processes.",
            "If workspaces fail to start, start Puddle from the Start menu or a normal terminal.",
        ),
        F::CodeIntegrityUnreadable => (
            "Puddle couldn't read which Windows rules decide what programs may run.",
            "If Runtime starts reports a blocked runtime, follow the steps there.",
        ),
        _ => UNWORDED,
    }
}

/// The network client, the disk and the folders that hold Puddle's data.
fn storage(os: Os, finding: Finding) -> (&'static str, &'static str) {
    use Finding as F;
    match finding {
        F::GlobalSecureAccessInstalled => (
            "Company resources you reach through Global Secure Access may not work from a workspace.",
            "If one fails from a workspace, ask your IT team for another way to reach it.",
        ),
        F::GlobalSecureAccessUnknown => (
            "Puddle couldn't tell whether a client is installed that keeps workspaces from company resources.",
            "If company resources fail from a workspace, check whether the Global Secure Access client is installed.",
        ),
        F::DiskSpaceLow => (
            "Workspaces may stop with unclear errors when the drive fills up.",
            "Free some space: delete workspaces and files you no longer need.",
        ),
        F::DiskSpaceCritical => (
            "Workspaces will soon stop with unclear errors: the drive is almost full.",
            "Free space now: delete workspaces and files you no longer need.",
        ),
        F::DiskSpaceUnreadable => (
            "Puddle can't warn you before the drive fills up.",
            "Check again later. If it stays, copy the report and send it with a bug report.",
        ),
        F::DataPathWritable => (
            "Puddle can't start any workspace: other accounts can change a folder above its data.",
            "Take the write permission for others off that folder with `chmod go-w`.",
        ),
        F::DataPathBlocked => (
            "Puddle can't start any workspace: a folder above its data is not a folder, or belongs to someone else.",
            "Fix or move that folder, or point XDG_DATA_HOME at a folder you own.",
        ),
        F::DataPathAcl => (
            "Puddle can't start any workspace: another account may change a folder above its data.",
            "Remove that account's access entry, or keep Puddle's data under a folder only you can enter.",
        ),
        F::DataPathUnreadable => (
            "Puddle couldn't check the folders above its data, which its runtime may refuse.",
            "If workspaces fail to start, check that only you can change those folders.",
        ),
        F::DiskNotEncrypted => (
            "Anyone who gets this disk, from a lost or stolen computer, can read Puddle's data and secrets.",
            match os {
                Os::Windows => "Turn on BitLocker or Device encryption for the drive.",
                Os::MacOs => "Turn on FileVault.",
                Os::Linux => "Encrypt the drive with LUKS.",
                Os::Other => "Encrypt the drive with your system's tools.",
            },
        ),
        F::SwapNotEncrypted => (
            "Secrets Puddle holds in memory can be written to swap, where anyone who gets the disk can read them.",
            match os {
                Os::Windows => {
                    "Run `fsutil behavior set encryptpagingfile 1` as administrator, then restart."
                }
                Os::Linux => "Give the swap a random key at each boot, or turn swap off.",
                Os::MacOs | Os::Other => "Encrypt the swap with your system's tools.",
            },
        ),
        F::DiskEncryptionUnknown => (
            "Puddle couldn't tell whether the disk is encrypted, so a lost computer may expose your secrets.",
            "Check in your system's settings that disk encryption is on.",
        ),
        _ => UNWORDED,
    }
}

/// SSH keys, the Azure CLI and the credential store.
fn ssh_azure_and_store(os: Os, finding: Finding) -> (&'static str, &'static str) {
    use Finding as F;
    match finding {
        F::SshAgentMissing => (
            "Programs in a workspace can't use your SSH keys for servers other than Git hosts. Git over SSH still works.",
            match os {
                Os::Windows => {
                    "Start the OpenSSH Authentication Agent service, or the SSH agent of 1Password or Bitwarden."
                }
                _ => "Start your SSH agent before Puddle, so SSH_AUTH_SOCK is set.",
            },
        ),
        F::SshAgentLocked => (
            "Programs in a workspace can't use your SSH keys while the agent is locked. Git over SSH still works.",
            "Unlock the agent, or add a key with ssh-add.",
        ),
        F::SshAgentEmpty => (
            "Programs in a workspace can't use SSH keys: the agent holds none. Git over SSH still works.",
            "Add a key with ssh-add, or in 1Password or Bitwarden.",
        ),
        F::SshAgentSlow => (
            "Programs in a workspace may wait for your SSH agent: it did not answer in time.",
            "Unlock the agent, then check again.",
        ),
        F::SshAgentUnusable => (
            "Programs in a workspace can't use your SSH keys: what Puddle reached is not an SSH agent.",
            "Check that the SSH agent setting names your agent's pipe or socket.",
        ),
        F::AzCliMissing => (
            "Workspaces can't sign in to Azure DevOps through `az`. Nothing else needs it.",
            "If you use Azure DevOps, install the Azure CLI and run `az login`.",
        ),
        F::AzCliUnreadable => (
            "Signing in to Azure DevOps through `az` may fail: `az --version` did not work.",
            "If you use Azure DevOps, reinstall the Azure CLI.",
        ),
        F::CredentialStoreMissing => (
            "Puddle can't save tokens or passwords: no credential store answers.",
            "Start your system's keyring, then check again.",
        ),
        F::CredentialStoreLocked => (
            "Puddle can't save tokens or passwords while the credential store is locked.",
            "Unlock your keyring, then check again.",
        ),
        F::CredentialStoreNoDesktop => (
            "After a reboot the keyring may stay locked, and Puddle can't read its saved secrets then.",
            no_desktop_remedy(crate::SERVE_BUILT_IN),
        ),
        F::CredentialStoreVaultUnreadable => (
            "Puddle can't read the secrets it saved in its file vault.",
            "Restore the vault file from a copy, or move it aside and enter the secrets again.",
        ),
        F::CredentialStoreRecordUnreadable => (
            "Puddle doesn't know where it keeps its secrets, so it can't read them.",
            "Restore `secrets/store.json` in the data folder from a copy.",
        ),
        F::CredentialStoreKeyRejected => (
            "Puddle can't open its file vault, so its saved secrets can't be read.",
            "Restore the Secure Boot setting if it changed. Otherwise move the vault aside and enter the secrets again.",
        ),
        F::CredentialStoreKeyUnopenable => (
            "Puddle can't open its file vault, so its saved secrets can't be read.",
            "Make sure `systemd-creds` of systemd 256 or later works, then check again.",
        ),
        F::CredentialStoreKeyFile => (
            "A copy of your config folder opens Puddle's vault of secrets.",
            "Use systemd 256 or later, which seals the key to this computer instead.",
        ),
        F::CredentialStoreTpmLibrariesMissing => (
            "The vault key isn't tied to this computer's TPM2 chip, so a copy of the disk is easier to open.",
            "Install the TPM2 libraries (tpm2-tools).",
        ),
        _ => UNWORDED,
    }
}

/// The one-line remedy for [`Finding::CredentialStoreNoDesktop`]; see `no_desktop_fix`.
pub(crate) fn no_desktop_remedy(serve: bool) -> &'static str {
    if serve {
        "On a server, run Puddle with `puddle serve --headless` so secrets go in its own vault."
    } else {
        "On a server, unlock the keyring after every reboot, then check again."
    }
}

#[cfg(test)]
mod tests;
