// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn host(name: &str) -> Host {
    Host::parse_normalised(name).unwrap_or_else(|_| unreachable!("a valid test name"))
}

fn named(name: &str) -> GitSshAccount {
    GitSshAccount::Named(name.to_owned())
}

#[tokio::test]
async fn the_answer_to_the_in_memory_connect_is_read_to_its_blank_line_and_no_further() {
    let mut answer = &b"HTTP/1.1 200 Connection Established\r\n\r\nTLS"[..];
    assert!(connected(&mut answer).await.is_ok());
    assert_eq!(answer, b"TLS");
    for bad in [&b""[..], b"HTTP/1.1 200 OK\r\n"] {
        let mut answer = bad;
        assert_eq!(
            connected(&mut answer).await.unwrap_err().message,
            "the connection to the Git host closed"
        );
    }
    let endless = vec![b'x'; MAX_ANSWER + 10];
    let mut answer = &endless[..];
    assert!(connected(&mut answer).await.is_err());
    assert_eq!(answer.len(), 10, "it stops at its limit");
}

#[test]
fn a_failure_on_the_way_names_the_step_and_the_host() {
    assert_eq!(
        failed("request to", &host("github.com"), &"reset").message,
        "the request to github.com failed: reset"
    );
}

#[test]
fn github_greets_by_name_with_its_own_words_and_status() {
    let said = greeting(&host("github.com"), &named("octocat"));
    assert_eq!(
        said.message,
        "Hi octocat! You've successfully authenticated, but GitHub does not provide shell access.\n"
    );
    assert_eq!(said.status, 1);
}

#[test]
fn gitlab_welcomes_with_status_0() {
    let said = greeting(&host("gitlab.com"), &named("octo"));
    assert_eq!(said.message, "Welcome to GitLab, @octo!\n");
    assert_eq!(said.status, 0);
    let said = greeting(&host("gitlab.com"), &GitSshAccount::Unnamed);
    assert_eq!(said.message, "Welcome to GitLab!\n");
    assert_eq!(said.status, 0);
}

#[test]
fn any_other_host_gets_the_github_form_with_its_name() {
    let said = greeting(&host("git.example.org"), &GitSshAccount::Unnamed);
    assert_eq!(
        said.message,
        "Hi! You've successfully authenticated, but git.example.org does not provide shell access.\n"
    );
    assert_eq!(said.status, 1);
}

#[test]
fn no_identity_is_the_failed_login_line() {
    let said = greeting(&host("github.com"), &GitSshAccount::NoIdentity);
    assert_eq!(said.status, 255);
    assert!(said.message.starts_with("puddle: no identity"));
}

#[test]
fn a_name_that_is_not_one_a_host_spells_is_left_out() {
    for bad in ["", "evil\u{1b}[2J", "two words", &"a".repeat(101)] {
        let said = greeting(&host("github.com"), &named(bad));
        assert!(said.message.starts_with("Hi! "), "{bad:?}");
    }
    for good in ["a", "octo-cat_1.x", &"a".repeat(100)] {
        let said = greeting(&host("github.com"), &named(good));
        assert!(
            said.message.starts_with(&format!("Hi {good}! ")),
            "{good:?}"
        );
    }
}
