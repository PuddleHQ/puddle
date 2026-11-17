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

#[test]
fn every_finding_is_worded_by_the_function_it_is_sent_to() {
    for os in [Os::Windows, Os::Linux, Os::MacOs, Os::Other] {
        for finding in ALL {
            let (impact, remedy) = finding_text(os, finding);
            assert_ne!((impact, remedy), UNWORDED, "{finding:?} on {os:?}");
            for text in [impact, remedy] {
                assert!(text.chars().count() <= 170, "{finding:?}: {text}");
                assert!(text.ends_with('.'), "{finding:?}: {text}");
                assert!(text.matches(". ").count() <= 1, "{finding:?}: {text}");
            }
        }
    }
}
