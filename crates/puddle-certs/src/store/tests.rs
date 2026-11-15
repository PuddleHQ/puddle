// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn sources_print_as_windows_store_paths() {
    let shown: Vec<String> = SOURCES.iter().map(ToString::to_string).collect();
    assert_eq!(
        shown,
        [
            "LocalMachine\\Root\\.Default",
            "LocalMachine\\Root\\.GroupPolicy",
            "LocalMachine\\Root\\.Enterprise",
            "CurrentUser\\Root\\.Default",
            "CurrentUser\\Root\\.GroupPolicy",
            "LocalMachine\\CA\\.Default",
            "LocalMachine\\CA\\.GroupPolicy",
            "LocalMachine\\CA\\.Enterprise",
            "CurrentUser\\CA\\.Default",
            "CurrentUser\\CA\\.GroupPolicy",
            "LocalMachine\\Disallowed",
            "CurrentUser\\Disallowed",
        ]
    );
}

#[test]
fn snapshot_keeps_certs_and_unreadable_stores_in_order() {
    let mut s = StoreSnapshot::new();
    s.add(SOURCES[0], vec![1]);
    s.add(SOURCES[3], vec![2]);
    s.note_unreadable(SOURCES[1], "access denied");
    assert_eq!(s.certs().len(), 2);
    assert_eq!(s.certs()[1].der, [2]);
    assert_eq!(s.unreadable()[0].source, SOURCES[1]);
    let err = StoreError::DisallowedUnreadable {
        store: SOURCES[10],
        reason: "error 5".into(),
    };
    assert_eq!(
        err.to_string(),
        "cannot read the LocalMachine\\Disallowed certificate store: error 5"
    );
}

#[cfg(not(windows))]
#[test]
fn other_hosts_have_no_stores() {
    assert_eq!(read_host_stores(), Ok(StoreSnapshot::new()));
}
