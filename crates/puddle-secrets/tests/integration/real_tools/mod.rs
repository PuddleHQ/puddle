// SPDX-License-Identifier: AGPL-3.0-or-later
//! The real GitHub CLI and Git Credential Manager, driven by Puddle's discovery and sign-in with
//! made-up tokens: no account and no secret.
//!
//! `gh` signs in to a stand-in GitHub Enterprise Server on `localhost` (see [`standin`]); Git
//! Credential Manager (GCM) holds `github.com` credentials in the computer's own store (Windows
//! Credential Manager on the CI runner). What the tools print is what Puddle parses, so a change in
//! either tool's output, or a hand-written fake that never matched it, shows here.
//!
//! The test changes the computer's credential stores, so it is `#[ignore]`d and runs only where the
//! job says so (`--run-ignored only`, with `PUDDLE_REAL_TOOLS_CONTRACT` set and `GH_CONFIG_DIR` and
//! `GIT_CONFIG_GLOBAL` pointing at folders of its own; `.github/workflows/real-tools.yml`). It
//! needs port 443 of `127.0.0.1`. Where the test
//! cannot trust the stand-in's certificate itself (not Windows), the job prepares one and names it in
//! `PUDDLE_STANDIN_CERT_PEM` and `PUDDLE_STANDIN_KEY_PEM`. `PUDDLE_REAL_GH_VERSION` and
//! `PUDDLE_REAL_GCM_VERSION` (from `ci/fetch-real-tools.sh`) name the versions the tools must report;
//! `PUDDLE_REAL_TOOLS_INSECURE_STORAGE` keeps `gh` tokens in its config file on a computer without a
//! keyring.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a panic is how a test fails"
)]

mod standin;

use std::time::Duration;

use puddle_secrets::{
    AccountName, Fetch, HostName, Listing, MemoryStore, SignIns, SourceSpec, Sources, ToolPaths,
    UrlPath, discover,
};
use standin::{HOST, StandIn, USER_CODE, token_of};

const GH_USERS: [&str; 3] = ["octocat", "monalisa", "expired"];
const GCM_USERS: [&str; 2] = ["octocat", "monalisa"];
const ORG: &str = "acme";
const ORG_USER: &str = "me@example.com";
const PATH: &str = "acme/widgets";

/// The test writes to the computer's credential stores, so it is `#[ignore]`d and the job runs it
/// with `--run-ignored only`; this makes sure the job also gave it stores of its own.
fn require_job_environment() {
    for name in [
        "PUDDLE_REAL_TOOLS_CONTRACT",
        "GH_CONFIG_DIR",
        "GIT_CONFIG_GLOBAL",
    ] {
        assert!(
            std::env::var_os(name).is_some(),
            "{name} must be set by the job (a folder or file of its own for the tools' configuration), so the \
             test never touches a real account's"
        );
    }
}

struct Out {
    ok: bool,
    stdout: String,
    stderr: String,
}

/// Runs a tool the way Puddle does (no prompt, no token from the environment), with `stdin`.
async fn tool(program: &str, args: &[&str], stdin: &str) -> Out {
    use tokio::io::AsyncWriteExt;
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args)
        .env("GCM_INTERACTIVE", "never")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GH_PROMPT_DISABLED", "1")
        .env_remove("GH_TOKEN")
        .env_remove("GITHUB_TOKEN")
        .env_remove("GH_ENTERPRISE_TOKEN")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = cmd
        .spawn()
        .unwrap_or_else(|e| panic!("cannot start {program}: {e}"));
    if let Some(mut input) = child.stdin.take() {
        // a tool that exits before reading its input is judged by its exit status below
        let _ = input.write_all(stdin.as_bytes()).await;
    }
    let out = child.wait_with_output().await.unwrap();
    Out {
        ok: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

async fn seed_ok(program: &str, args: &[&str], stdin: &str) -> String {
    let out = tool(program, args, stdin).await;
    assert!(out.ok, "{program} {args:?} failed: {}", out.stderr);
    out.stdout
}

/// Makes the computer trust the stand-in's certificate for as long as this lives.
struct Trusted(Option<String>);

impl Trusted {
    fn new(standin: &StandIn) -> Self {
        let Some(cn) = standin.generated_cn.clone() else {
            return Self(None);
        };
        #[cfg(windows)]
        {
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("stand-in.pem");
            std::fs::write(&file, &standin.cert_pem).unwrap();
            let added = std::process::Command::new("certutil")
                .args(["-addstore", "-f", "Root"])
                .arg(&file)
                .output()
                .unwrap();
            assert!(
                added.status.success(),
                "certutil could not trust the stand-in (the job must run as an administrator): {}",
                String::from_utf8_lossy(&added.stdout)
            );
            Self(Some(cn))
        }
        #[cfg(not(windows))]
        {
            panic!(
                "this computer cannot trust a certificate from inside the test ({cn}): prepare one and set \
                 PUDDLE_STANDIN_CERT_PEM and PUDDLE_STANDIN_KEY_PEM"
            );
        }
    }
}

impl Drop for Trusted {
    fn drop(&mut self) {
        #[cfg(windows)]
        if let Some(cn) = &self.0 {
            // best effort: the runner is thrown away, and the name is unique to this run
            let _ = std::process::Command::new("certutil")
                .args(["-delstore", "Root", cn])
                .output();
        }
        #[cfg(not(windows))]
        let _ = &self.0;
    }
}

/// Signs the made-up accounts out again, so a computer that is not a runner is left as it was.
struct Cleanup;

impl Drop for Cleanup {
    fn drop(&mut self) {
        let run = |program: &str, args: &[&str], stdin: &str| {
            use std::io::Write;
            let Ok(mut child) = std::process::Command::new(program)
                .args(args)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
            else {
                return;
            };
            if let Some(mut input) = child.stdin.take() {
                // the tool may already have exited; this is best effort
                let _ = input.write_all(stdin.as_bytes());
            }
            // the exit status is not needed
            let _ = child.wait();
        };
        for user in GH_USERS.into_iter().chain(["newuser"]) {
            run(
                "gh",
                &["auth", "logout", "--hostname", HOST, "--user", user],
                "",
            );
        }
        for user in GCM_USERS {
            run(
                "git",
                &["credential", "reject"],
                &format!("protocol=https\nhost=github.com\nusername={user}\n\n"),
            );
        }
        run(
            "git",
            &["-c", "credential.useHttpPath=true", "credential", "reject"],
            &format!("protocol=https\nhost=github.com\npath={PATH}\nusername=octocat\n\n"),
        );
        run(
            "git",
            &["credential-manager", "azure-repos", "unbind", ORG],
            "",
        );
    }
}

fn gh_login_args(extra: &'static [&'static str]) -> Vec<&'static str> {
    let mut args = vec!["auth", "login", "--hostname", HOST, "--with-token"];
    args.extend_from_slice(extra);
    args
}

async fn check_versions() {
    if let Ok(want) = std::env::var("PUDDLE_REAL_GH_VERSION") {
        let got = seed_ok("gh", &["--version"], "").await;
        assert!(
            got.contains(&format!("gh version {want} ")),
            "the gh on PATH is not the pinned {want}: {got}"
        );
    }
    if let Ok(want) = std::env::var("PUDDLE_REAL_GCM_VERSION") {
        let got = seed_ok("git", &["credential-manager", "--version"], "").await;
        assert!(
            got.starts_with(&want),
            "the GCM `git` runs is not the pinned {want}: {got}"
        );
    }
}

async fn seed_gh() {
    let extra: &'static [&'static str] =
        if std::env::var_os("PUDDLE_REAL_TOOLS_INSECURE_STORAGE").is_some() {
            &["--insecure-storage"]
        } else {
            &[]
        };
    for user in GH_USERS {
        seed_ok(
            "gh",
            &gh_login_args(extra),
            &format!("{}\n", token_of(user)),
        )
        .await;
    }
}

async fn seed_gcm() {
    for user in GCM_USERS {
        let input = format!(
            "protocol=https\nhost=github.com\nusername={user}\npassword=ghp_madeup_{user}\n\n"
        );
        seed_ok("git", &["credential", "approve"], &input).await;
    }
    // a credential for one repository, as Puddle asks for it (`credential.useHttpPath`)
    let input = format!(
        "protocol=https\nhost=github.com\npath={PATH}\nusername=octocat\npassword=ghp_madeup_octocat\n\n"
    );
    seed_ok(
        "git",
        &["-c", "credential.useHttpPath=true", "credential", "approve"],
        &input,
    )
    .await;
    seed_ok(
        "git",
        &["credential-manager", "azure-repos", "bind", ORG, ORG_USER],
        "",
    )
    .await;
}

type Row = (Listing, String, String, Option<String>, bool);

async fn found(tools: &ToolPaths) -> (Vec<Row>, String) {
    let discovery = discover(tools).await;
    assert!(discovery.problems.is_empty(), "{:?}", discovery.problems);
    let rows = discovery
        .accounts
        .iter()
        .map(|a| {
            (
                a.via,
                a.host.to_string(),
                a.account.to_string(),
                a.org.as_ref().map(ToString::to_string),
                a.signed_in,
            )
        })
        .collect();
    (rows, format!("{discovery:?}"))
}

fn row(via: Listing, host: &str, account: &str, org: Option<&str>, signed_in: bool) -> Row {
    (
        via,
        host.to_owned(),
        account.to_owned(),
        org.map(str::to_owned),
        signed_in,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "changes the computer's credential stores: .github/workflows/real-tools.yml runs it with --run-ignored only"]
async fn the_real_gh_and_git_credential_manager_are_found_read_and_signed_in_with_made_up_tokens() {
    require_job_environment();
    check_versions().await;
    let standin = StandIn::start().await;
    let _trusted = Trusted::new(&standin);
    let _cleanup = Cleanup;
    seed_gh().await;
    standin.revoke("expired");
    seed_gcm().await;

    // Discovery: the names and states the two tools print, as Puddle reads them.
    let tools = ToolPaths::resolve();
    let (rows, debug) = found(&tools).await;
    assert_eq!(
        rows,
        [
            row(Listing::GhAuthStatus, HOST, "expired", None, false),
            row(Listing::GhAuthStatus, HOST, "monalisa", None, true),
            row(Listing::GhAuthStatus, HOST, "octocat", None, true),
            row(Listing::GcmGithub, "github.com", "monalisa", None, true),
            row(Listing::GcmGithub, "github.com", "octocat", None, true),
            row(
                Listing::GcmAzureRepos,
                "dev.azure.com",
                ORG_USER,
                Some(ORG),
                true
            ),
        ]
    );
    assert!(
        !debug.contains("madeup"),
        "a token reached the discovery result: {debug}"
    );
    let asked = standin.requests();
    assert!(
        asked.iter().any(|r| r == "GET /api/v3/"),
        "gh did not ask the stand-in host: {asked:?}"
    );

    // Each found account maps to a source that reads its token from the real tool.
    let sources = Sources::new(tools.clone(), std::sync::Arc::new(MemoryStore::new()));
    let host = HostName::new(HOST).unwrap();
    for user in ["octocat", "monalisa"] {
        let spec = SourceSpec::Gh {
            host: host.clone(),
            account: AccountName::new(user).unwrap(),
        };
        let got = sources.fetch(&spec).await.unwrap();
        assert_eq!(
            got.credential.secret.expose(),
            token_of(user),
            "gh token of {user}"
        );
    }
    let git_spec = SourceSpec::GitCredential {
        host: HostName::new("github.com").unwrap(),
        path: UrlPath::new(PATH).unwrap(),
        username: Some(AccountName::new("octocat").unwrap()),
    };
    let got = sources.fetch(&git_spec).await.unwrap();
    assert_eq!(got.credential.secret.expose(), "ghp_madeup_octocat");

    // Sign-in with gh: the code and the address gh prints, then the account appears once approved.
    let sign_ins = SignIns::new(tools.clone()).with_window(Duration::from_secs(60));
    let new = SourceSpec::Gh {
        host: host.clone(),
        account: AccountName::new("newuser").unwrap(),
    };
    let start = match sign_ins.begin(&new).await {
        Ok(start) => start,
        Err(err) => {
            // Show what gh itself prints, so a change in its wording is read off the log.
            standin.approve("diag");
            let said = tool("gh", &["auth", "login", "--hostname", HOST, "--web"], "").await;
            panic!(
                "sign-in did not start: {err:?}; gh prints stdout {:?} stderr {:?}",
                said.stdout, said.stderr
            );
        }
    };
    assert_eq!(start.code.as_deref(), Some(USER_CODE));
    assert_eq!(start.url, Some(format!("https://{HOST}/login/device")));
    standin.approve("newuser");
    puddle_test_kit::eventually_async(
        "the account approved at the stand-in shows in gh",
        Duration::from_secs(60),
        || async {
            let (rows, _) = found(&tools).await;
            rows.contains(&row(Listing::GhAuthStatus, HOST, "newuser", None, true))
        },
    )
    .await;
    let got = sources.fetch(&new).await.unwrap();
    assert_eq!(got.credential.secret.expose(), token_of("newuser"));

    // Sign-in with a Git credential the helper already holds: it answers at once.
    let start = sign_ins.begin(&git_spec).await.unwrap();
    assert_eq!((start.code, start.url), (None, None));
}
