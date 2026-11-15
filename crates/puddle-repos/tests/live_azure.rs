// SPDX-License-Identifier: AGPL-3.0-or-later
//! The real Azure DevOps answering a token it does not know, over Puddle's own route (direct, the
//! platform's trust store). No account is involved: the tokens are made up, and the call is one an
//! anonymous client could make.
//!
//! The offline tests (`lists/azure.rs`) script the three forms Azure DevOps is reported to refuse a bad
//! token with (`203` and a sign-in page, `200` and HTML, a redirect to the sign-in) and check that each
//! gives `token_rejected`. These tests ask the service itself, so a change in what it answers shows up
//! here. They need the internet and a third party's service, so they are `#[ignore]`d: the
//! `live-contracts` workflow runs them nightly, and a pull request never waits for them.
#![expect(
    clippy::unwrap_used,
    clippy::print_stderr,
    reason = "a failed step fails the test; what the service answered goes to stderr for the log"
)]

use std::collections::BTreeSet;
use std::sync::Arc;

use futures_util::future::BoxFuture;
use puddle_repos::{
    Api, ApiRequest, Authorization, Freshness, HostApi, ListState, ProblemKind, Read, Repos,
    Secrets,
};
use puddle_secrets::{
    Credential, HostName, OrgName, Secret, SourceError, SourceSpec, StoredId, TokenScope,
};
use puddle_store::{
    Author, Coverage, CredentialBinding, Identity, IdentityDraft, Limits, ManualClock, Owner, Store,
};
use puddle_upstream::{Chain, Config, Discovery, FakeOs, NoAuth, ProxyConfig, TlsClient};

/// An organisation that exists and is public, so the service answers the token and not the name.
const ORGANISATION: &str = "microsoft";
/// Made up, shaped like a personal access token (52 base32 characters).
const PAT: &str = "CANARYadopat0000000000000000000000000000000000abcd";
/// Made up, shaped like a Microsoft Entra access token (three base64url parts, the first `eyJ`).
fn entra() -> &'static str {
    // Built from parts so no literal in the repository is a token the secret scanner reads as a JWT.
    const PARTS: [&str; 3] = [
        "eyJhbGciOiJSUzI1NiJ9",
        "eyJzdWIiOiJDQU5BUlktYWRvIn0",
        "Y2FuYXJ5LXNpZ25hdHVyZQ",
    ];
    static TOKEN: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    TOKEN.get_or_init(|| PARTS.join("."))
}

fn live_api() -> Arc<HostApi> {
    let chain = Chain::new(
        Discovery::new(FakeOs::new(ProxyConfig::default()), Config::default()),
        Arc::new(NoAuth),
    );
    Arc::new(HostApi::new(chain, TlsClient::new([]).unwrap()))
}

fn list_request(organisation: &str, authorization: Authorization) -> ApiRequest {
    ApiRequest {
        host: "dev.azure.com".to_owned(),
        path: format!("/{organisation}/_apis/git/repositories?api-version=7.1"),
        accept: "application/json",
        headers: &[],
        authorization,
        if_none_match: None,
    }
}

/// Every token a source is asked for is the one made up here.
struct Made(&'static str);

impl Secrets for Made {
    fn get<'a>(&'a self, _: &'a SourceSpec) -> BoxFuture<'a, Result<Credential, SourceError>> {
        Box::pin(async move {
            Ok(Credential {
                username: None,
                secret: Arc::new(Secret::new(self.0.to_owned())),
            })
        })
    }

    fn invalidate(&self, _: &SourceSpec) {}
}

fn identity(organisation: &str) -> Identity {
    let host = HostName::new("dev.azure.com").unwrap();
    let source = SourceSpec::Stored {
        id: StoredId::new("tok-1").unwrap(),
        scope: TokenScope {
            host: host.clone(),
            org: Some(OrgName::new(organisation).unwrap()),
        },
    };
    let owners = BTreeSet::from([Owner::new(organisation).unwrap()]);
    let binding =
        CredentialBinding::new(&host, source, Coverage::new(owners, false).unwrap()).unwrap();
    Store::open_in_memory(Arc::new(ManualClock::new(0)), Limits::default())
        .unwrap()
        .create_identity(
            puddle_store::UserId::OWNER,
            IdentityDraft {
                label: "Work".to_owned(),
                author: Author::new("Work", "me@example.org").unwrap(),
                credentials: vec![binding],
            },
        )
        .unwrap()
}

/// How often a question is asked while the service answers something it has no rule for.
const ATTEMPTS: u32 = 4;

/// What the service made of `token`. A server error (`503` and the like) says the service was busy,
/// not how it refuses a bad token, so only that answer is asked again, a few seconds apart; a
/// service that keeps failing still fails the test with its last answer in the log.
async fn problem_for(token: &'static str, organisation: &str) -> Option<ProblemKind> {
    let mut attempt = 1;
    // flake-ok: asks the service again after a server error (an operation retried, with the last answer returned), not a wait for a condition.
    loop {
        let problem = ask(token, organisation).await;
        if problem != Some(ProblemKind::BadAnswer) || attempt == ATTEMPTS {
            return problem;
        }
        attempt += 1;
        tokio::time::sleep(std::time::Duration::from_secs(u64::from(attempt) * 2)).await;
    }
}

async fn ask(token: &'static str, organisation: &str) -> Option<ProblemKind> {
    let repos = Repos::new(
        live_api(),
        Arc::new(Made(token)),
        Arc::new(ManualClock::new(0)),
    );
    let lists = repos
        .lists(
            &[identity(organisation)],
            Read {
                only: None,
                freshness: Freshness::Cached,
            },
        )
        .await;
    let list = lists.first().unwrap();
    eprintln!(
        "{organisation}: state {:?}, problem {:?}",
        list.state,
        list.problem.as_ref().map(|p| (p.kind, p.message.as_str()))
    );
    assert_eq!(list.state, ListState::Failed);
    list.problem.as_ref().map(|p| p.kind)
}

#[tokio::test]
#[ignore = "asks the real Azure DevOps over the internet; the nightly live-contracts workflow runs it"]
async fn azure_devops_refuses_a_made_up_personal_access_token_with_a_form_the_list_reads_as_rejected()
 {
    let reply = live_api()
        .get(list_request(
            ORGANISATION,
            Authorization::BasicPassword(Arc::new(Secret::new(PAT.to_owned()))),
        ))
        .await
        .unwrap();
    eprintln!(
        "answered {} {:?} location {:?}",
        reply.status,
        reply.header("content-type"),
        reply.header("location").map(|l| l.split('?').next())
    );
    let json = reply
        .header("content-type")
        .is_some_and(|t| t.starts_with("application/json"));
    assert!(
        matches!(reply.status, 203 | 302 | 401) || (reply.status == 200 && !json),
        "a made-up token got {} with content-type {:?}: a form the list does not know",
        reply.status,
        reply.header("content-type")
    );

    assert_eq!(
        problem_for(PAT, ORGANISATION).await,
        Some(ProblemKind::TokenRejected)
    );
}

#[tokio::test]
#[ignore = "asks the real Azure DevOps over the internet; the nightly live-contracts workflow runs it"]
async fn azure_devops_refuses_a_made_up_entra_token_as_a_bearer_the_same_way() {
    assert_eq!(
        problem_for(entra(), ORGANISATION).await,
        Some(ProblemKind::TokenRejected)
    );
}

#[tokio::test]
#[ignore = "asks the real Azure DevOps over the internet; the nightly live-contracts workflow runs it"]
async fn an_organisation_that_does_not_exist_is_not_found_and_not_a_rejected_token() {
    assert_eq!(
        problem_for(PAT, "puddle-contract-test-no-such-organisation").await,
        Some(ProblemKind::NotFound)
    );
}
