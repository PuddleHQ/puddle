// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn a_named_endpoint_wins_on_both_systems() {
    for windows in [true, false] {
        let got = AgentEndpoint::resolve(
            windows,
            Some(r"  \\.\pipe\pageant.alice  "),
            Some(OsStr::new("/run/agent")),
        );
        assert_eq!(got, Some(AgentEndpoint::at(r"\\.\pipe\pageant.alice")));
    }
}

#[test]
fn windows_falls_back_to_the_openssh_pipe_whatever_the_environment_says() {
    for configured in [None, Some(""), Some("  ")] {
        let got = AgentEndpoint::resolve(true, configured, Some(OsStr::new("/run/agent")));
        assert_eq!(got, Some(AgentEndpoint::at(WINDOWS_DEFAULT_PIPE)));
    }
}

#[test]
fn elsewhere_the_environment_names_the_socket() {
    let got = AgentEndpoint::resolve(false, None, Some(OsStr::new("/run/user/1000/agent.sock")));
    assert_eq!(got, Some(AgentEndpoint::at("/run/user/1000/agent.sock")));
    assert_eq!(AgentEndpoint::resolve(false, Some(" "), None), None);
    assert_eq!(
        AgentEndpoint::resolve(false, None, Some(OsStr::new(""))),
        None
    );
}

#[test]
fn for_host_takes_the_configured_endpoint_whatever_the_environment_holds() {
    assert_eq!(
        AgentEndpoint::for_host(Some("/configured/agent.sock")),
        Some(AgentEndpoint::at("/configured/agent.sock"))
    );
}

#[test]
fn it_shows_its_path() {
    let e = AgentEndpoint::at("/a/b");
    assert_eq!(e.to_string(), "/a/b");
    assert_eq!(e.path(), Path::new("/a/b"));
}
