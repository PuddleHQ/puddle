// SPDX-License-Identifier: AGPL-3.0-or-later
use super::clone_detail::{clone_detail, url_host};

const GIT: &str = "git exited 128: fatal: unable to access 'https://forge.corp.example/acme/api.git/': \
                   SSL certificate problem: unable to get local issuer certificate";

#[test]
fn an_untrusted_server_certificate_is_named_before_gits_own_words() {
    let detail = clone_detail(
        GIT,
        true,
        "",
        "https://forge.corp.example/acme/api.git",
        "api",
    );
    assert_eq!(
        detail,
        format!(
            "Windows does not trust forge.corp.example's certificate: add your company's root \
             certificate to Windows. Git said: {GIT}. The repository stays on the list: use Clone \
             again to try once more, or Remove from the list. To get it yourself, clone it in a \
             terminal in the workspace: git clone https://forge.corp.example/acme/api.git \
             /workspace/api"
        )
    );
}

#[test]
fn any_other_failure_keeps_the_reason_first() {
    let detail = clone_detail(
        "git exited 128: Could not resolve host",
        false,
        "The workspace was made without it. ",
        "https://x.example/a.git",
        "a",
    );
    assert!(
        detail.starts_with(
            "git exited 128: Could not resolve host. The workspace was made without it. The repository"
        ),
        "{detail}"
    );
}

#[test]
fn the_host_of_a_repository_address_drops_user_port_and_path() {
    assert_eq!(
        url_host("https://forge.example/a/b.git"),
        Some("forge.example")
    );
    assert_eq!(
        url_host("https://me@forge.example:8443/a"),
        Some("forge.example")
    );
    assert_eq!(url_host("https://forge.example"), Some("forge.example"));
    assert_eq!(url_host("not a url"), None);
    assert_eq!(url_host("https:///x"), None);
}
