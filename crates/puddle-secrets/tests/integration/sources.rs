// SPDX-License-Identifier: AGPL-3.0-or-later
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code: a panic is how a test fails"
)]
//! The sources against fake `gh` and `git` executables and an in-memory store. Nothing here reads
//! the user's real credentials.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::common;

use common::{CANARY, Fakes};
use puddle_secrets::{
    AccountName, Fetch, HostName, MemoryStore, OrgName, Refusal, Secret, SecretCache, SecretStore,
    SignInNeeded, SourceError, SourceSpec, Sources, StoredId, TenantId, TokenScope, Tool,
    ToolPaths, UrlPath,
};

fn sources(gh: Option<PathBuf>, git: Option<PathBuf>) -> Sources {
    Sources::new(ToolPaths::new(gh, git), Arc::new(MemoryStore::new()))
}

fn gh_spec() -> SourceSpec {
    SourceSpec::Gh {
        host: HostName::new("github.com").unwrap(),
        account: AccountName::new("me").unwrap(),
    }
}

fn git_spec(path: &str, user: Option<&str>) -> SourceSpec {
    SourceSpec::GitCredential {
        host: HostName::new("dev.azure.com").unwrap(),
        path: UrlPath::new(path).unwrap(),
        username: user.map(|u| AccountName::new(u).unwrap()),
    }
}

#[tokio::test]
async fn gh_reads_the_named_account_without_prompting() {
    let fakes = Fakes::new();
    let gh = fakes.install("gh", &format!("[]\nstdout={CANARY}-gh\\n\n"));
    let got = sources(Some(gh.clone()), None)
        .fetch(&gh_spec())
        .await
        .unwrap();
    assert_eq!(got.credential.secret.expose(), format!("{CANARY}-gh"));
    assert_eq!(got.valid_for, None);
    let log = Fakes::log(&gh);
    assert!(
        log.contains("ARGS auth token --hostname github.com --user me\n"),
        "{log}"
    );
    assert!(log.contains("GH_PROMPT_DISABLED=1"), "{log}");
    assert!(
        log.contains("GCM_INTERACTIVE=never GIT_TERMINAL_PROMPT=0 GIT_ASKPASS= "),
        "{log}"
    );
}

#[tokio::test]
async fn gh_failures_are_typed() {
    let fakes = Fakes::new();
    let not_in = fakes.install("gh", "[]\nexit=1\n");
    let err = sources(Some(not_in), None)
        .fetch(&gh_spec())
        .await
        .unwrap_err();
    assert_eq!(err, SourceError::NotSignedIn);

    let fakes = Fakes::new();
    let empty = fakes.install("gh", "[]\nstdout=\\n\n");
    let err = sources(Some(empty), None)
        .fetch(&gh_spec())
        .await
        .unwrap_err();
    assert_eq!(err, SourceError::NotSignedIn);

    let fakes = Fakes::new();
    let spaced = fakes.install("gh", &format!("[]\nstdout={CANARY} two words\n"));
    let err = sources(Some(spaced), None)
        .fetch(&gh_spec())
        .await
        .unwrap_err();
    assert_eq!(err, SourceError::Refused(Refusal::Malformed));

    let err = sources(None, None).fetch(&gh_spec()).await.unwrap_err();
    assert_eq!(err, SourceError::ToolMissing(Tool::Gh));
    assert!(!err.needs_sign_in());
}

#[tokio::test]
async fn a_tool_that_hangs_times_out_and_asks_for_sign_in() {
    let fakes = Fakes::new();
    let slow = fakes.install("gh", "[]\nsleep_ms=5000\nstdout=late\n");
    let src = sources(Some(slow), None).with_timeout(Duration::from_millis(300));
    let err = src.fetch(&gh_spec()).await.unwrap_err();
    assert_eq!(err, SourceError::Timeout(Tool::Gh));
    assert!(err.needs_sign_in());
}

#[tokio::test]
async fn git_credential_always_sends_the_path_and_checks_the_answer() {
    let fakes = Fakes::new();
    let git = fakes.install(
        "git",
        &format!(
            "[credential fill]\necho_stdin=1\nstdout=username=me\\npassword={CANARY}-git\\n\n"
        ),
    );
    let src = sources(None, Some(git.clone()));
    let got = src.fetch(&git_spec("org", None)).await.unwrap();
    assert_eq!(got.credential.secret.expose(), format!("{CANARY}-git"));
    assert_eq!(got.credential.username.as_deref(), Some("me"));
    let log = Fakes::log(&git);
    assert!(
        log.contains("ARGS -c credential.useHttpPath=true credential fill\n"),
        "{log}"
    );
    assert!(
        log.contains("STDIN protocol=https\\nhost=dev.azure.com\\npath=org\\n\\n\n"),
        "the request must carry protocol, host and path: {log}"
    );
    assert!(
        log.contains("GCM_INTERACTIVE=never GIT_TERMINAL_PROMPT=0 GIT_ASKPASS= "),
        "{log}"
    );

    let got = src
        .fetch(&git_spec("org/project", Some("me")))
        .await
        .unwrap();
    assert_eq!(got.credential.username.as_deref(), Some("me"));
    assert!(Fakes::log(&git).contains("path=org/project\\nusername=me\\n\\n"));
}

#[tokio::test]
async fn git_credential_answer_for_another_entry_is_not_used() {
    let fakes = Fakes::new();
    // A helper that ignores the path and answers with another organisation's entry.
    let other = fakes.install(
        "git",
        &format!("[]\nstdout=protocol=https\\nhost=dev.azure.com\\npath=client\\nusername=me\\npassword={CANARY}\\n\n"),
    );
    let err = sources(None, Some(other))
        .fetch(&git_spec("org", None))
        .await
        .unwrap_err();
    assert_eq!(err, SourceError::Refused(Refusal::WrongTarget));
    assert!(!format!("{err} {err:?}").contains(CANARY));

    let fakes = Fakes::new();
    let wrong_user = fakes.install(
        "git",
        &format!("[]\necho_stdin=1\nstdout=username=you\\npassword={CANARY}\\n\n"),
    );
    let err = sources(None, Some(wrong_user))
        .fetch(&git_spec("org", Some("me")))
        .await
        .unwrap_err();
    assert_eq!(err, SourceError::Refused(Refusal::WrongTarget));
}

#[tokio::test]
async fn git_credential_without_a_stored_login_is_not_signed_in() {
    let fakes = Fakes::new();
    // Git with prompts off exits 128: "could not read Username: terminal prompts disabled".
    let git = fakes.install("git", "[]\nexit=128\n");
    let err = sources(None, Some(git))
        .fetch(&git_spec("org", None))
        .await
        .unwrap_err();
    assert_eq!(err, SourceError::NotSignedIn);
    let err = sources(None, None)
        .fetch(&git_spec("org", None))
        .await
        .unwrap_err();
    assert_eq!(err, SourceError::ToolMissing(Tool::Git));
}

fn stored_spec(id: &str) -> SourceSpec {
    SourceSpec::Stored {
        id: StoredId::new(id).unwrap(),
        scope: TokenScope {
            host: HostName::new("dev.azure.com").unwrap(),
            org: Some(OrgName::new("acme").unwrap()),
        },
    }
}

#[tokio::test]
async fn stored_tokens_come_from_the_store() {
    let store = Arc::new(MemoryStore::new());
    let src = Sources::new(
        ToolPaths::default(),
        Arc::clone(&store) as Arc<dyn SecretStore>,
    );
    let spec = stored_spec("t1");
    assert_eq!(
        src.fetch(&spec).await.unwrap_err(),
        SourceError::NotSignedIn
    );
    store
        .set(
            &StoredId::new("t1").unwrap(),
            &Secret::new(format!("{CANARY}-pat")),
        )
        .unwrap();
    let got = src.fetch(&spec).await.unwrap();
    assert_eq!(got.credential.secret.expose(), format!("{CANARY}-pat"));
    assert_eq!(spec.scope().org.unwrap().as_str(), "acme");
    store.break_it();
    assert_eq!(
        src.fetch(&spec).await.unwrap_err(),
        SourceError::StoreUnavailable
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_burst_of_misses_is_one_tool_run() {
    let fakes = Fakes::new();
    let gh = fakes.install("gh", &format!("[]\nsleep_ms=200\nstdout={CANARY}\\n\n"));
    let cache = Arc::new(SecretCache::new(sources(Some(gh.clone()), None)));
    let spec = gh_spec();
    let mut tasks = Vec::new();
    for _ in 0..256 {
        let (cache, spec) = (Arc::clone(&cache), spec.clone());
        tasks.push(tokio::spawn(async move { cache.get(&spec).await }));
    }
    for t in tasks {
        assert_eq!(t.await.unwrap().unwrap().secret.expose(), CANARY);
    }
    assert_eq!(Fakes::log(&gh).matches("ARGS ").count(), 1);
}

#[tokio::test]
async fn a_failed_read_that_needs_the_user_raises_a_sign_in_notice() {
    let fakes = Fakes::new();
    let gh = fakes.install("gh", "[]\nexit=1\n");
    let cache = SecretCache::new(sources(Some(gh), None));
    let mut events = cache.subscribe();
    assert_eq!(
        cache.get(&gh_spec()).await.unwrap_err(),
        SourceError::NotSignedIn
    );
    // flake-ok: the cache sends the notice with a synchronous `broadcast::send` before `get` returns,
    // and `get` was awaited, so the notice is already in the channel.
    assert_eq!(
        events.try_recv().unwrap(),
        SignInNeeded { source: gh_spec() }
    );
}

#[test]
fn tools_are_found_on_an_absolute_search_path_only() {
    let fakes = Fakes::new();
    let gh = fakes.install("gh", "[]\n");
    let dir = fakes.dir.path();
    let found = ToolPaths::from_search_path(dir.as_os_str());
    assert_eq!(found, ToolPaths::new(Some(gh.clone()), None));
    let relative = ToolPaths::from_search_path(std::ffi::OsStr::new("."));
    assert_eq!(relative, ToolPaths::default());
    let joined = std::env::join_paths([Path::new("relative-dir"), dir]).unwrap();
    assert_eq!(
        ToolPaths::from_search_path(&joined),
        ToolPaths::new(Some(gh), None)
    );
}

#[cfg(unix)]
#[test]
fn a_file_without_the_execute_bit_is_not_a_tool() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("gh"), "#!/bin/sh\n").unwrap();
    assert_eq!(
        ToolPaths::from_search_path(dir.path().as_os_str()),
        ToolPaths::default()
    );
}

/// The real OS store, through a target no real credential uses. It runs where a store exists (the
/// Windows job, a desktop session) and says so where it does not, unless
/// `PUDDLE_REQUIRE_OS_CREDENTIAL_STORE` is set (the Windows job sets it): then a missing store fails.
#[test]
fn the_os_store_round_trips_a_test_owned_entry() {
    if !puddle_secrets::keyring_available() {
        assert!(
            std::env::var_os("PUDDLE_REQUIRE_OS_CREDENTIAL_STORE").is_none(),
            "PUDDLE_REQUIRE_OS_CREDENTIAL_STORE is set but this computer has no credential store that works"
        );
        eprintln!("no OS credential store here; skipped");
        return;
    }
    let store = puddle_secrets::KeyringStore::new();
    let id = StoredId::new(format!("selftest-{}", std::process::id())).unwrap();
    store.set(&id, &Secret::new(CANARY.to_owned())).unwrap();
    assert_eq!(store.get(&id).unwrap().unwrap().expose(), CANARY);
    store.delete(&id).unwrap();
    assert!(store.get(&id).unwrap().is_none());
    store.delete(&id).unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn a_tool_that_is_not_allowed_to_run_is_not_reported_missing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gh");
    std::fs::write(&path, "not a program").unwrap();
    let err = sources(Some(path), None)
        .fetch(&gh_spec())
        .await
        .unwrap_err();
    assert_eq!(err, SourceError::CouldNotRun(Tool::Gh));
}

const TENANT: &str = "72f988bf-86f1-41af-91ab-2d7cd011db47";

fn az_spec() -> SourceSpec {
    SourceSpec::AzCli {
        tenant: TenantId::new(TENANT).unwrap(),
        org: Some(OrgName::new("acme").unwrap()),
    }
}

fn az_sources(az: Option<PathBuf>) -> Sources {
    Sources::new(
        ToolPaths::new(None, None).with_az(az),
        Arc::new(MemoryStore::new()),
    )
}

fn az_answer(tenant: &str) -> String {
    format!(
        "[]\nstdout={{\"accessToken\":\"{CANARY}-az\",\"expires_on\":9999999999,\"tenant\":\"{tenant}\"}}\\n\n"
    )
}

#[tokio::test]
async fn az_cli_uses_the_fixed_arguments() {
    let fakes = Fakes::new();
    let az = fakes.install("az", &az_answer(TENANT));
    let got = az_sources(Some(az.clone()))
        .fetch(&az_spec())
        .await
        .unwrap();
    assert_eq!(got.credential.secret.expose(), format!("{CANARY}-az"));
    assert!(got.valid_for.is_some());
    let log = Fakes::log(&az);
    assert!(
        log.contains(&format!(
            "ARGS account get-access-token --resource 499b84ac-1321-427f-aa17-267ca6975798 --tenant {TENANT} --output json\n"
        )),
        "{log}"
    );
    assert_eq!(az_spec().scope().host.as_str(), "dev.azure.com");
}

#[tokio::test]
async fn az_cli_refuses_an_answer_for_another_tenant() {
    let fakes = Fakes::new();
    let az = fakes.install("az", &az_answer("11111111-2222-3333-4444-555555555555"));
    let err = az_sources(Some(az)).fetch(&az_spec()).await.unwrap_err();
    assert_eq!(err, SourceError::WrongTenant);
}

#[tokio::test]
async fn az_cli_exit_one_with_login_hint_is_not_signed_in() {
    let fakes = Fakes::new();
    let az = fakes.install(
        "az",
        "[]\nexit=1\nstderr=ERROR: Please run 'az login' to setup account.\\n\n",
    );
    let err = az_sources(Some(az)).fetch(&az_spec()).await.unwrap_err();
    assert_eq!(err, SourceError::NotSignedIn);
    assert!(err.needs_sign_in());
}

#[tokio::test]
async fn az_cli_tells_each_missing_piece_apart() {
    let fakes = Fakes::new();
    let wrong = fakes.install(
        "az",
        "[]\nexit=1\nstderr=AADSTS90002: Tenant not found.\\n\n",
    );
    assert_eq!(
        az_sources(Some(wrong)).fetch(&az_spec()).await.unwrap_err(),
        SourceError::WrongTenant
    );
    let fakes = Fakes::new();
    let noaccess = fakes.install("az", "[]\nexit=1\nstderr=AADSTS65001: no consent\\n\n");
    assert_eq!(
        az_sources(Some(noaccess))
            .fetch(&az_spec())
            .await
            .unwrap_err(),
        SourceError::NoAccess
    );
    assert_eq!(
        az_sources(None).fetch(&az_spec()).await.unwrap_err(),
        SourceError::ToolMissing(Tool::Az)
    );
    let fakes = Fakes::new();
    let broken = fakes.install("az", "[]\nexit=2\nstderr=Traceback boom\\n\n");
    assert_eq!(
        az_sources(Some(broken))
            .fetch(&az_spec())
            .await
            .unwrap_err(),
        SourceError::CouldNotRun(Tool::Az)
    );
    let fakes = Fakes::new();
    let slow = fakes.install("az", "[]\nsleep_ms=5000\n");
    let err = az_sources(Some(slow))
        .with_timeout(Duration::from_millis(300))
        .fetch(&az_spec())
        .await
        .unwrap_err();
    assert_eq!(err, SourceError::Timeout(Tool::Az));
}

#[tokio::test]
async fn az_cli_stderr_never_reaches_the_error() {
    let fakes = Fakes::new();
    let az = fakes.install("az", &format!("[]\nexit=2\nstderr={CANARY}-stderr\\n\n"));
    let err = az_sources(Some(az)).fetch(&az_spec()).await.unwrap_err();
    assert!(!format!("{err} {err:?}").contains(CANARY));
}

#[tokio::test]
async fn az_cli_is_cached_between_requests() {
    let fakes = Fakes::new();
    let az = fakes.install("az", &az_answer(TENANT));
    let cache = SecretCache::new(az_sources(Some(az.clone())));
    for _ in 0..3 {
        let got = cache.get(&az_spec()).await.unwrap();
        assert_eq!(got.secret.expose(), format!("{CANARY}-az"));
    }
    assert_eq!(Fakes::log(&az).matches("ARGS ").count(), 1);
}
