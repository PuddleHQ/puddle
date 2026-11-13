// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn seconds_has_one_decimal() {
    assert_eq!(seconds(Duration::from_millis(2345)), "2.3 s");
    assert_eq!(seconds(Duration::from_millis(40)), "0.0 s");
}

#[test]
fn hex_only_for_windows_status_codes() {
    assert_eq!(hex_or_dec(Os::Windows, -1_073_741_515), "0xC0000135");
    assert_eq!(hex_or_dec(Os::Windows, 3), "3");
    assert_eq!(hex_or_dec(Os::Linux, -1), "-1");
}

#[test]
fn every_system_integrity_code_is_app_control() {
    let msb = Path::new("msb");
    for code in (4550..=4562).chain(4580..=4582).chain([577]) {
        let b = spawn_failure(Os::Windows, msb, Some(code)).unwrap();
        assert_eq!(b.finding, Finding::BlockedByAppControl, "{code}");
    }
    assert!(spawn_failure(Os::Windows, msb, Some(4563)).is_none());
    assert!(spawn_failure(Os::Windows, msb, Some(2)).is_none());
    assert!(spawn_failure(Os::Windows, msb, None).is_none());
}

#[test]
fn windows_codes_mean_nothing_elsewhere() {
    let msb = Path::new("msb");
    assert!(spawn_failure(Os::Linux, msb, Some(1260)).is_none());
    assert!(load_failure(Os::Linux, msb, -1_073_741_515, true).is_none());
    assert_eq!(
        spawn_failure(Os::Linux, msb, Some(13)).unwrap().finding,
        Finding::RuntimePermissions
    );
}

#[test]
fn load_failures_by_status() {
    let msb = Path::new("msb");
    let f = |status: u32| {
        #[expect(
            clippy::cast_possible_wrap,
            reason = "test: NTSTATUS as Windows reports it"
        )]
        let code = status as i32;
        load_failure(Os::Windows, msb, code, true).map(|b| b.finding)
    };
    assert_eq!(f(0xC000_0428), Some(Finding::BlockedByAppControl));
    assert_eq!(f(0xC000_0022), Some(Finding::ProcessCreationDenied));
    assert_eq!(f(0xC000_0906), Some(Finding::BlockedByAntivirus));
    assert_eq!(f(0xC000_0138), Some(Finding::DllMissing));
    assert_eq!(f(0xC000_0139), Some(Finding::DllMissing));
    assert_eq!(f(0xC000_0005), None);
}

#[test]
fn a_missing_azure_cli_only_warns() {
    let check = az_cli(&AzCliFacts::NotFound);
    assert_eq!(check.id, CheckId::AzCli);
    assert_eq!(check.status, Status::Warn);
    assert_eq!(check.finding, Some(Finding::AzCliMissing));
    assert!(check.summary.contains("needed only for Azure DevOps"));
    assert_eq!(
        az_cli(&AzCliFacts::Found {
            version: "2.64.0".into()
        })
        .status,
        Status::Ok
    );
}

#[test]
fn an_azure_cli_whose_version_cannot_be_read_only_warns() {
    let check = az_cli(&AzCliFacts::Unknown {
        detail: "exited with Some(1)".into(),
    });
    assert_eq!(check.status, Status::Warn);
    assert_eq!(check.finding, None);
    assert!(check.summary.contains("exited with Some(1)"));
    assert!(check.fix.is_some());
}
