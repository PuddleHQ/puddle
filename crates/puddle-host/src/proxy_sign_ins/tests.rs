// SPDX-License-Identifier: AGPL-3.0-or-later
use puddle_secrets::MemoryStore;

use super::*;

#[test]
fn each_way_a_sign_in_is_refused_reaches_the_api_as_itself() {
    assert_eq!(
        ProxySignInError::from(ProxyPasswordError::BadUser),
        ProxySignInError::BadUser
    );
    assert_eq!(
        ProxySignInError::from(ProxyPasswordError::BadPassword),
        ProxySignInError::BadPassword
    );
    assert_eq!(
        ProxySignInError::from(ProxyPasswordError::Store),
        ProxySignInError::Store
    );
}

#[test]
fn a_value_basic_cannot_carry_is_refused_by_the_stored_sign_ins_with_the_api_error() {
    let passwords = ProxyPasswords::new(Arc::new(MemoryStore::new()));
    let proxy = ProxyAddr::new("proxy.corp", 8080);
    let bad_user = passwords
        .set(&proxy, "a:b", &puddle_types::fixture::password("fine"))
        .unwrap_err();
    assert_eq!(ProxySignInError::from(bad_user), ProxySignInError::BadUser);
    let empty = String::new();
    let bad_password = passwords.set(&proxy, "alice", &empty).unwrap_err();
    assert_eq!(
        ProxySignInError::from(bad_password),
        ProxySignInError::BadPassword
    );
}

#[test]
fn a_lost_sign_in_is_a_problem_naming_the_proxy_until_it_is_entered_again() {
    let dir = tempfile::tempdir().unwrap();
    let known = dir.path().join("proxy-sign-ins");
    let proxy = ProxyAddr::new("proxy.corp", 8080);
    let password = puddle_types::fixture::password("kept");
    ProxyPasswords::with_known_file(Arc::new(MemoryStore::new()), known.clone())
        .set(&proxy, "alice", &password)
        .unwrap();

    let problems = Problems::new(Arc::new(puddle_types::NullSink));
    let reset = ProxyPasswords::with_known_file(Arc::new(MemoryStore::new()), known);
    reset.load().unwrap();
    report_lost(&reset, &problems);
    let raised = problems.list();
    assert_eq!(raised.len(), 1, "{raised:?}");
    assert!(raised[0].detail.contains("proxy.corp:8080"), "{raised:?}");
    puddle_types::fixture::assert_absent("the problem", &raised[0].detail, &password);

    reset.set(&proxy, "alice", &password).unwrap();
    report_lost(&reset, &problems);
    assert_eq!(problems.list(), []);
}
