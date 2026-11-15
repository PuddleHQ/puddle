// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn the_stored_word_is_the_user_name() {
    for user in [SessionUser::Agent, SessionUser::Root] {
        let json = serde_json::to_string(&user).unwrap();
        assert_eq!(json, format!("\"{}\"", user.name()));
        assert_eq!(serde_json::from_str::<SessionUser>(&json).unwrap(), user);
        assert_eq!(user.to_string(), user.name());
    }
    assert!(serde_json::from_str::<SessionUser>("\"vscode\"").is_err());
    assert!(SessionUser::Root.is_root() && !SessionUser::Agent.is_root());
}

#[test]
fn agent_lives_in_home_agent_and_root_in_root() {
    assert_eq!(SessionUser::Agent.home(), "/home/agent");
    assert_eq!(SessionUser::Root.home(), "/root");
    assert_eq!(SessionUser::default(), SessionUser::Agent);
}
