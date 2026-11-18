// SPDX-License-Identifier: AGPL-3.0-or-later
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code: a panic is how a test fails"
)]
//! A GitHub account found in Git Credential Manager, read and signed in to through real Git, with
//! a stand-in for the manager that keeps its entries the way it does in the Windows credential
//! store (see the fake tool's `gcm`). Nothing here reads the user's real credentials.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::common;

use common::{CANARY, Fakes};
use puddle_secrets::{
    AccountName, Fetch, HostName, MemoryStore, SignIns, SourceError, SourceSpec, Sources,
    ToolPaths, UrlPath,
};
use puddle_test_kit::eventually;

/// The source "Found on this computer" makes of the GitHub account `me` that the manager lists.
fn found_account() -> SourceSpec {
    SourceSpec::GitCredential {
        host: HostName::new("github.com").unwrap(),
        path: UrlPath::new("me").unwrap(),
        username: Some(AccountName::new("me").unwrap()),
    }
}

/// A scratch Git configuration whose one credential helper is the manager stand-in over `store`,
/// and a `git` that runs real Git with it.
struct Manager {
    _fakes: Fakes,
    scratch: tempfile::TempDir,
    git: PathBuf,
}

impl Manager {
    fn new(entries: &str, sign_in: Option<&str>) -> Self {
        let fakes = Fakes::new();
        let scratch = tempfile::tempdir().unwrap();
        let store = scratch.path().join("store.tsv");
        std::fs::write(&store, entries).unwrap();
        let global = scratch.path().join("gitconfig");
        std::fs::write(&global, "").unwrap();
        let sign_in = sign_in.map_or_else(String::new, |s| format!("gcm_sign_in={s}\n"));
        let helper = fakes.install(
            "git-credential-manager",
            &format!("[]\ngcm_store={}\n{sign_in}", store.display()),
        );
        // Git runs a helper through a shell, which reads backslashes as escapes.
        let helper = helper.display().to_string().replace('\\', "/");
        let git = fakes.install(
            "git",
            &format!(
                "[credential]\nrun={}\nenv=GIT_CONFIG_NOSYSTEM=1\nenv=GIT_CONFIG_GLOBAL={}\n\
                 env=GIT_CONFIG_COUNT=1\nenv=GIT_CONFIG_KEY_0=credential.helper\nenv=GIT_CONFIG_VALUE_0={helper}\n",
                real_git().display(),
                global.display(),
            ),
        );
        Self {
            _fakes: fakes,
            scratch,
            git,
        }
    }

    fn sources(&self) -> Sources {
        Sources::new(
            ToolPaths::new(None, Some(self.git.clone())),
            Arc::new(MemoryStore::new()),
        )
    }

    /// The store's targets and users, without the secrets.
    fn entries(&self) -> Vec<String> {
        read_entries(&self.scratch.path().join("store.tsv"))
    }
}

fn read_entries(store: &Path) -> Vec<String> {
    std::fs::read_to_string(store)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let mut parts = l.split('\t');
            Some(format!("{} {}", parts.next()?, parts.next()?))
        })
        .collect()
}

/// The manager keeps a browser sign-in to github.com for the whole host under the account's name,
/// which is how Git asks for it on a clone. The account the manager lists reads from that entry.
#[tokio::test]
async fn a_found_github_account_reads_the_entry_git_itself_uses() {
    let manager = Manager::new(&format!("git:https://github.com\tme\t{CANARY}-me\n"), None);
    let got = manager.sources().fetch(&found_account()).await.unwrap();
    assert_eq!(got.credential.secret.expose(), format!("{CANARY}-me"));
    assert_eq!(got.credential.username.as_deref(), Some("me"));
}

/// With two accounts on the host, the manager keeps the second one under a target that names it;
/// each found account reads its own.
#[tokio::test]
async fn a_found_github_account_beside_another_reads_its_own_entry() {
    let manager = Manager::new(
        &format!(
            "git:https://github.com\twork\t{CANARY}-work\n\
             git:https://me@github.com\tme\t{CANARY}-me\n"
        ),
        None,
    );
    let got = manager.sources().fetch(&found_account()).await.unwrap();
    assert_eq!(got.credential.secret.expose(), format!("{CANARY}-me"));
}

/// No entry for the account: a read from a workspace never prompts, it needs a sign-in.
#[tokio::test]
async fn a_found_github_account_without_an_entry_needs_a_sign_in() {
    let manager = Manager::new(
        &format!("git:https://github.com\twork\t{CANARY}-work\n"),
        None,
    );
    let err = manager.sources().fetch(&found_account()).await.unwrap_err();
    assert_eq!(err, SourceError::NotSignedIn);
}

/// A sign-in from Puddle is kept where Git and the next read find it: the account's entry for the
/// whole host, not a second entry for a path Git never asks for.
#[tokio::test]
async fn signing_in_keeps_the_answer_where_git_and_the_next_read_find_it() {
    let manager = Manager::new("", Some(&format!("me:{CANARY}-signed-in")));
    SignIns::new(ToolPaths::new(None, Some(manager.git.clone())))
        .begin(&found_account())
        .await
        .unwrap();
    let entries = eventually(
        "the manager to keep the sign-in",
        Duration::from_secs(10),
        || {
            let entries = manager.entries();
            if entries.is_empty() {
                Err("no entry yet")
            } else {
                Ok(entries)
            }
        },
    )
    .await;
    assert_eq!(entries, ["git:https://github.com me"]);
    let got = manager.sources().fetch(&found_account()).await.unwrap();
    assert_eq!(
        got.credential.secret.expose(),
        format!("{CANARY}-signed-in")
    );
}

/// The real `git` on the path (`git.exe` on Windows).
pub(crate) fn real_git() -> PathBuf {
    let name = if cfg!(windows) { "git.exe" } else { "git" };
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
        .expect("git on the path")
}
