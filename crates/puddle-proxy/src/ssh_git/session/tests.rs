// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;

#[test]
fn a_git_protocol_value_is_a_list_of_the_three_versions_and_nothing_else() {
    assert_eq!(protocol_versions("version=2"), Some(vec![2]));
    assert_eq!(protocol_versions("version=1:version=2"), Some(vec![1, 2]));
    assert_eq!(protocol_versions("version=0"), Some(vec![0]));
    for bad in [
        "",
        "version=3",
        "version=2:",
        "version=2:foo",
        "VERSION=2",
        "version=2 ",
        "version=0:version=1:version=2:version=2",
        "version=2\nx=1",
    ] {
        assert_eq!(protocol_versions(bad), None, "{bad:?}");
    }
}

#[test]
fn version_0_is_the_absence_of_a_git_protocol() {
    assert_eq!(git_protocol(&[]), None);
    assert_eq!(git_protocol(&[0]), None);
    assert_eq!(git_protocol(&[2]).as_deref(), Some("version=2"));
    assert_eq!(
        git_protocol(&[0, 1, 2]).as_deref(),
        Some("version=1:version=2")
    );
}

#[test]
fn libssh2_is_told_by_its_name_anywhere_in_what_the_guest_sent_first() {
    assert!(names_libssh2(b"SSH-2.0-libssh2_1.11.0\r\n"));
    assert!(names_libssh2(b"xxlibssh2"));
    assert!(!names_libssh2(b"SSH-2.0-OpenSSH_9.6\r\n"));
    assert!(!names_libssh2(b"libssh"));
    assert!(!names_libssh2(b""));
}

#[test]
fn the_server_answers_as_puddle_with_no_delays_and_no_idle_timeout() {
    let config = server_config(Vec::new());
    assert!(
        matches!(&config.server_id, russh::SshId::Standard(id) if id == "SSH-2.0-Puddle"),
        "{:?}",
        config.server_id
    );
    assert_eq!(config.auth_rejection_time, std::time::Duration::ZERO);
    assert_eq!(
        config.auth_rejection_time_initial,
        Some(std::time::Duration::ZERO)
    );
    assert_eq!(config.inactivity_timeout, None);
}
