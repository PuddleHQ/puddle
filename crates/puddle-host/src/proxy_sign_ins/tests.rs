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
    let bad_user = passwords.set(&proxy, "a:b", &"p".repeat(2)).unwrap_err();
    assert_eq!(ProxySignInError::from(bad_user), ProxySignInError::BadUser);
    let empty = "x".repeat(2).replace('x', "");
    let bad_password = passwords.set(&proxy, "alice", &empty).unwrap_err();
    assert_eq!(
        ProxySignInError::from(bad_password),
        ProxySignInError::BadPassword
    );
}
