// SPDX-License-Identifier: AGPL-3.0-or-later
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code: a panic is how a test fails"
)]
//! The listing commands, against fake tools that also print a token-shaped canary.

use crate::common;

use common::{CANARY, Fakes};
use puddle_secrets::{Listing, SourceError, Tool, ToolPaths, discover};

fn gh_json() -> String {
    format!(
        r#"{{"hosts":{{"github.com":[{{"login":"me","active":true,"state":"success","token":"{CANARY}"}},{{"login":"work","active":false,"state":"error"}}]}}}}"#
    )
}

#[tokio::test]
async fn discovery_runs_only_the_fixed_listings_and_keeps_names() {
    let fakes = Fakes::new();
    // `gh auth status` exits 1 when one account is broken but still prints the JSON.
    let gh = fakes.install(
        "gh",
        &format!("[auth status --json hosts]\nexit=1\nstdout={}\n", gh_json()),
    );
    let git = fakes.install(
        "git",
        &format!(
            "[github list]\nstdout=me\\ntoken {CANARY}\\n\n\
             [azure-repos list]\nstdout=acme:\\n  (global) -> me@example.com\\n  password={CANARY}\\n\n"
        ),
    );
    let found = discover(&ToolPaths::new(Some(gh.clone()), Some(git.clone()))).await;
    assert!(found.problems.is_empty(), "{:?}", found.problems);
    let rows: Vec<_> = found
        .accounts
        .iter()
        .map(|a| {
            (
                a.via,
                a.host.as_str(),
                a.account.as_str(),
                a.org.as_ref().map(puddle_secrets::OrgName::as_str),
                a.signed_in,
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            (Listing::GhAuthStatus, "github.com", "me", None, true),
            (Listing::GhAuthStatus, "github.com", "work", None, false),
            (Listing::GcmGithub, "github.com", "me", None, true),
            (
                Listing::GcmAzureRepos,
                "dev.azure.com",
                "me@example.com",
                Some("acme"),
                true
            ),
        ]
    );
    assert!(!format!("{found:?}").contains(CANARY));

    let gh_log = Fakes::log(&gh);
    assert_eq!(gh_log.matches("ARGS ").count(), 1);
    assert!(
        gh_log.contains("ARGS auth status --json hosts\n"),
        "{gh_log}"
    );
    let git_log = Fakes::log(&git);
    assert!(
        git_log.contains("ARGS credential-manager github list\n"),
        "{git_log}"
    );
    assert!(
        git_log.contains("ARGS credential-manager azure-repos list\n"),
        "{git_log}"
    );
    assert_eq!(git_log.matches("ARGS ").count(), 2);
}

#[tokio::test]
async fn a_missing_tool_or_a_logged_out_cli_is_a_problem_not_a_failure() {
    let fakes = Fakes::new();
    let gh = fakes.install("gh", "[]\nexit=1\n");
    let found = discover(&ToolPaths::new(Some(gh), None)).await;
    assert_eq!(found.accounts.len(), 0);
    assert!(
        found
            .problems
            .contains(&(Listing::GhAuthStatus, SourceError::NotSignedIn))
    );
    assert!(
        found
            .problems
            .contains(&(Listing::GcmGithub, SourceError::ToolMissing(Tool::Git)))
    );
    assert!(
        found
            .problems
            .contains(&(Listing::GcmAzureRepos, SourceError::ToolMissing(Tool::Git)))
    );
}

#[tokio::test]
async fn git_credential_manager_lists_are_read_from_standard_error_too() {
    // GCM 3.0.2 prints both `list` commands on standard error and leaves standard output empty.
    let fakes = Fakes::new();
    let git = fakes.install(
        "git",
        &format!(
            "[github list]\nstderr=me\\nwarning: a diagnostic line\\n\n\
             [azure-repos list]\nstderr=acme:\\n  (global) -> me@example.com\\n\nstdout=token {CANARY}\\n\n"
        ),
    );
    let found = discover(&ToolPaths::new(None, Some(git))).await;
    let rows: Vec<_> = found
        .accounts
        .iter()
        .map(|a| {
            (
                a.via,
                a.account.as_str(),
                a.org.as_ref().map(puddle_secrets::OrgName::as_str),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            (Listing::GcmGithub, "me", None),
            (Listing::GcmAzureRepos, "me@example.com", Some("acme")),
        ]
    );
    assert!(!format!("{found:?}").contains(CANARY));
}

#[tokio::test]
async fn standard_error_is_not_read_for_gh_or_for_a_failed_gcm_listing() {
    let fakes = Fakes::new();
    // gh's JSON is on standard output; a line on standard error must not be glued to it.
    let gh = fakes.install(
        "gh",
        "[auth status --json hosts]\nstdout={\"hosts\":{\"github.com\":[{\"login\":\"me\"}]}}\nstderr={\"hosts\":{}}\\n\n",
    );
    // a failed GCM listing is not read, whatever it printed
    let git = fakes.install("git", "[github list]\nexit=1\nstderr=ghost\\n\n");
    let found = discover(&ToolPaths::new(Some(gh), Some(git))).await;
    let names: Vec<_> = found.accounts.iter().map(|a| a.account.as_str()).collect();
    assert_eq!(names, ["me"]);
}
