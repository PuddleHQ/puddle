// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

/// Every finding, as [`finding_text`] sorts them.
const ALL: [Finding; 59] = [
    Finding::FirmwareVirtualizationOff,
    Finding::WhpNotEnabled,
    Finding::HypervisorNotRunning,
    Finding::NestedVirtualizationOff,
    Finding::RunsInVirtualMachine,
    Finding::VirtualizationUnknown,
    Finding::HypervisorQueryFailed,
    Finding::KvmMissing,
    Finding::KvmAccessDenied,
    Finding::UnsupportedOs,
    Finding::RuntimeMissing,
    Finding::RuntimeVersionMismatch,
    Finding::RuntimeDigestMismatch,
    Finding::RuntimeUnreadable,
    Finding::RuntimePermissions,
    Finding::RuntimeOverridden,
    Finding::BlockedByAppLocker,
    Finding::BlockedByAppControl,
    Finding::BlockedByAntivirus,
    Finding::ProcessCreationDenied,
    Finding::DllMissing,
    Finding::LaunchFailed,
    Finding::LaunchExited,
    Finding::LaunchTimedOut,
    Finding::BootFailed,
    Finding::BootTimedOut,
    Finding::BootBlockedByJob,
    Finding::BootSetupFailed,
    Finding::JobWithoutBreakaway,
    Finding::JobUnreadable,
    Finding::CodeIntegrityUnreadable,
    Finding::GlobalSecureAccessInstalled,
    Finding::GlobalSecureAccessUnknown,
    Finding::DiskSpaceLow,
    Finding::DiskSpaceCritical,
    Finding::DiskSpaceUnreadable,
    Finding::DataPathWritable,
    Finding::DataPathBlocked,
    Finding::DataPathAcl,
    Finding::DataPathUnreadable,
    Finding::DiskNotEncrypted,
    Finding::SwapNotEncrypted,
    Finding::DiskEncryptionUnknown,
    Finding::SshAgentMissing,
    Finding::SshAgentLocked,
    Finding::SshAgentEmpty,
    Finding::SshAgentSlow,
    Finding::SshAgentUnusable,
    Finding::AzCliMissing,
    Finding::AzCliUnreadable,
    Finding::CredentialStoreMissing,
    Finding::CredentialStoreLocked,
    Finding::CredentialStoreNoDesktop,
    Finding::CredentialStoreVaultUnreadable,
    Finding::CredentialStoreRecordUnreadable,
    Finding::CredentialStoreKeyRejected,
    Finding::CredentialStoreKeyUnopenable,
    Finding::CredentialStoreKeyFile,
    Finding::CredentialStoreTpmLibrariesMissing,
];

/// Every check, for the texts of a pass and of a check that did not run.
const CHECKS: [CheckId; 14] = [
    CheckId::Virtualization,
    CheckId::Hypervisor,
    CheckId::CodeIntegrity,
    CheckId::Runtime,
    CheckId::Launch,
    CheckId::TestBoot,
    CheckId::JobObject,
    CheckId::GlobalSecureAccess,
    CheckId::DiskSpace,
    CheckId::DataPath,
    CheckId::DiskEncryption,
    CheckId::SshAgent,
    CheckId::AzCli,
    CheckId::CredentialStore,
];

/// At most two short sentences.
fn assert_short(what: &str, text: &str) {
    assert!(!text.trim().is_empty(), "{what}: empty");
    assert!(text.chars().count() <= 170, "{what}: {text}");
    assert!(text.ends_with('.'), "{what}: {text}");
    assert!(text.matches(". ").count() <= 1, "{what}: {text}");
}

#[test]
fn every_check_says_what_a_pass_and_a_skip_mean_in_short() {
    for id in CHECKS {
        assert_short(&format!("{id:?} ok"), ok_impact(id));
        assert_short(&format!("{id:?} skipped"), skipped_impact(id));
    }
    assert_short("skipped remedy", SKIPPED_REMEDY);
    assert_short("unworded impact", UNWORDED.0);
    assert_short("unworded remedy", UNWORDED.1);
}

#[test]
fn every_finding_is_worded_by_the_function_it_is_sent_to() {
    for os in [Os::Windows, Os::Linux, Os::MacOs, Os::Other] {
        for finding in ALL {
            let (impact, remedy) = finding_text(os, finding);
            assert_ne!((impact, remedy), UNWORDED, "{finding:?} on {os:?}");
            assert_short(&format!("{finding:?} impact"), impact);
            assert_short(&format!("{finding:?} remedy"), remedy);
        }
    }
}
