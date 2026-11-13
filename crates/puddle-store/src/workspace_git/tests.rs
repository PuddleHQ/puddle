// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn repo(text: &str) -> RepoRef {
    let (host, rest) = text.split_once('/').unwrap();
    let (owner, name) = rest.split_once('/').unwrap();
    RepoRef::new(host, owner, name).unwrap()
}

fn entry(text: &str, pull: bool, push: bool) -> RepoEntry {
    RepoEntry {
        id: 1,
        repo: repo(text),
        pull,
        push,
        created_at: 0,
    }
}

#[test]
fn clone_addresses_name_the_repository_the_ui_also_reads() {
    // The same file drives the UI's parser (`ui/src/lib/identities/repo.ts`).
    let vectors: serde_json::Value =
        serde_json::from_str(include_str!("../../tests/data/repo-urls.json")).unwrap();
    for case in vectors["cases"].as_array().unwrap() {
        let url = case["url"].as_str().unwrap();
        let got = RepoRef::from_https_url(url);
        match case["repo"].as_array() {
            Some(want) => {
                let want: Vec<&str> = want.iter().map(|v| v.as_str().unwrap()).collect();
                let got = got.unwrap_or_else(|e| panic!("{url:?}: {e}"));
                assert_eq!(
                    [got.host.as_str(), got.owner.as_str(), got.repo.as_str()],
                    [want[0], want[1], want[2]],
                    "{url:?}"
                );
            }
            None => assert!(got.is_err(), "{url:?} must be refused, got {got:?}"),
        }
    }
}

#[test]
fn an_address_of_a_known_shape_needs_its_git_segment() {
    for bad in [
        "https://dev.azure.com/org/project/repo",
        "https://dev.azure.com/org/project/other/repo",
        "https://org.visualstudio.com/project/other/repo",
        "https://org.visualstudio.com/DefaultCollection/project/other/repo",
    ] {
        let err = RepoRef::from_https_url(bad).unwrap_err();
        assert_eq!(err.to_string(), "not an HTTPS repository address", "{bad}");
    }
    // The address of a host with an owner and no repository.
    let err = RepoRef::from_https_url("https://github.com/acme").unwrap_err();
    assert_eq!(
        err.to_string(),
        "not an HTTPS repository address",
        "an owner alone is no repository"
    );
    assert_eq!(
        RepoRef::from_https_url("https://dev.azure.com/org/_GIT/repo")
            .unwrap()
            .repo,
        "repo/repo"
    );
}

#[test]
fn an_escape_in_an_address_that_separates_or_controls_is_refused() {
    for escape in [
        "%00", "%01", "%1f", "%7f", "%2f", "%2F", "%5c", "%25", "%3f", "%23", "%3b",
    ] {
        let url = format!("https://github.com/acme/web{escape}x");
        let err = RepoRef::from_https_url(&url).unwrap_err();
        // Refused as an address, not later as a name that holds the character.
        assert_eq!(err.to_string(), "not an HTTPS repository address", "{url}");
    }
    // Escapes for ordinary characters decode.
    let got = RepoRef::from_https_url("https://github.com/acme/we%62%2d1").unwrap();
    assert_eq!(got.repo, "web-1");
    let got = RepoRef::from_https_url("https://dev.azure.com/org/p%20q/_git/r").unwrap();
    assert_eq!(got.repo, "p q/r");
}

#[test]
fn repos_are_canonical() {
    assert_eq!(repo("GitHub.com/Acme/Web.GIT"), repo("github.com/acme/web"));
    assert_eq!(repo("dev.azure.com/Org/Proj/Repo").repo, "proj/repo");
    assert_eq!(
        repo("github.com/acme/web").to_string(),
        "github.com/acme/web"
    );
    for bad in [
        "", ".", "..", "a//b", "a/b/c", "a b", "a%2Fb", "a\\b", "/a", "a/", ".git",
    ] {
        assert!(RepoRef::new("github.com", "acme", bad).is_err(), "{bad:?}");
    }
    assert!(RepoRef::new("github.com", "ac me", "x").is_err());
    assert!(RepoRef::new("git hub", "acme", "x").is_err());
    assert!(RepoRef::new("github.com", "acme", &"a".repeat(MAX_SEGMENT + 1)).is_err());
}

#[test]
fn an_azure_devops_name_can_hold_a_space_and_a_letter_a_git_host_has_not() {
    let spaced = RepoRef::new("dev.azure.com", "Contoso", "My Project/The Api").unwrap();
    assert_eq!(spaced.repo, "my project/the api");
    assert_eq!(
        spaced.to_string(),
        "dev.azure.com/contoso/my project/the api"
    );
    assert_eq!(
        RepoRef::new("contoso.visualstudio.com", "contoso", "Été/Api")
            .unwrap()
            .repo,
        "Été/api"
    );
    // Not at the ends of a name, and never what ends or splits a path segment.
    for bad in [
        "a /b", "a/ b", "a\u{0}/b", "a%/b", "a?/b", "a#/b", "a;/b", "a\\/b",
    ] {
        assert!(
            RepoRef::new("dev.azure.com", "contoso", bad).is_err(),
            "{bad:?}"
        );
    }
    // Another host keeps the plain alphabet.
    for bad in ["my project", "\u{e9}t\u{e9}", "a:b"] {
        assert!(RepoRef::new("github.com", "acme", bad).is_err(), "{bad:?}");
    }
}

#[test]
fn only_ascii_letters_fold_so_a_look_alike_is_another_name() {
    let kelvin = RepoRef::new("dev.azure.com", "org", "proj\u{212a}/x").unwrap();
    assert_ne!(
        kelvin,
        RepoRef::new("dev.azure.com", "org", "projk/x").unwrap()
    );
    assert_eq!(
        RepoRef::new("dev.azure.com", "org", "\u{c9}T\u{c9}/X")
            .unwrap()
            .repo,
        "\u{c9}t\u{c9}/x"
    );
}

#[test]
fn the_switches_decide_what_the_toggles_mean() {
    let mut git = WorkspaceGit::unconfigured();
    git.repos = vec![
        entry("github.com/a/b", true, false),
        entry("github.com/a/c", false, true),
    ];
    let (b, c, d) = (
        repo("github.com/a/b"),
        repo("github.com/a/c"),
        repo("github.com/a/d"),
    );
    // Defaults: push only to listed repos with Push on; pull anywhere.
    assert!(!git.allows_push(&b) && git.allows_push(&c) && !git.allows_push(&d));
    assert!(git.allows_pull(&b) && git.allows_pull(&c) && git.allows_pull(&d));
    git.only_pull_listed = true;
    assert!(git.allows_pull(&b) && !git.allows_pull(&c) && !git.allows_pull(&d));
    git.only_push_listed = false;
    assert!(git.allows_push(&b) && git.allows_push(&d));
}
