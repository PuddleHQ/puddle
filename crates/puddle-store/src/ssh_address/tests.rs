// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use crate::RepoRef;

#[test]
fn ssh_addresses_name_the_repository_the_ui_also_reads() {
    // The same file drives the UI's reader (`ui/src/lib/workspaces/model.ts`).
    let vectors: serde_json::Value =
        serde_json::from_str(include_str!("../../tests/data/repo-urls.json")).unwrap();
    for case in vectors["ssh_cases"].as_array().unwrap() {
        let url = case["url"].as_str().unwrap();
        assert!(looks_like_ssh(url), "{url:?} is written as SSH");
        let https = ssh_address_as_https(url);
        match case["https"].as_str() {
            Some(want) => {
                let https = https.unwrap_or_else(|e| panic!("{url:?}: {e}"));
                assert_eq!(https, want, "{url:?}");
                let got = RepoRef::from_https_url(&https).unwrap();
                let want: Vec<&str> = case["repo"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_str().unwrap())
                    .collect();
                assert_eq!(
                    [got.host.as_str(), got.owner.as_str(), got.repo.as_str()],
                    [want[0], want[1], want[2]],
                    "{url:?}"
                );
            }
            None => match case["refusal"].as_str().unwrap() {
                "host" => assert_eq!(https, Err(SshAddressError::OtherHost), "{url:?}"),
                "shape" => assert_eq!(https, Err(SshAddressError::Unreadable), "{url:?}"),
                "shape_repo" => {
                    let https = https.unwrap_or_else(|e| panic!("{url:?}: {e}"));
                    assert!(RepoRef::from_https_url(&https).is_err(), "{url:?}");
                }
                other => panic!("unknown refusal {other}"),
            },
        }
    }
}

#[test]
fn only_ssh_addresses_look_like_ssh() {
    for ssh in ["git@host:path", "host:path", "ssh://h/p", "SSH://h/p"] {
        assert!(looks_like_ssh(ssh), "{ssh}");
    }
    for other in [
        "https://github.com/a/b",
        "HTTPS://github.com/a/b",
        "http://github.com/a/b",
        "C:\\repos\\api",
        "C:/repos/api",
        "/home/me/repo",
        "",
        "ssh",
    ] {
        assert!(!looks_like_ssh(other), "{other}");
    }
}

#[test]
fn the_messages_say_what_to_use_instead() {
    let host = SshAddressError::OtherHost.to_string();
    assert!(
        host.contains("GitHub, GitLab, Bitbucket and Azure DevOps"),
        "{host}"
    );
    assert!(host.contains("HTTPS URL"), "{host}");
    let shape = SshAddressError::Unreadable.to_string();
    assert!(shape.contains("git@github.com:owner/repo.git"), "{shape}");
}
