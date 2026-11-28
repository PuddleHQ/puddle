// SPDX-License-Identifier: AGPL-3.0-or-later
//! What the injector decides for a request on a decrypted Git host: the workspace's own header
//! passes unchanged, a request with none gets the credential of the identity that covers its
//! owner, the repository table gates every push and (when asked) pull, and every refusal says
//! why in one line.
#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]

#[path = "../support/mod.rs"]
mod support;

use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use puddle_inject::testing::{OnlyListed, StaticSettings, World};
use puddle_inject::{CredentialSource, GitInjector, StoreSettings};
use puddle_proxy::{InjectContext, InjectDecision, Injector as _, RequestView};
use puddle_secrets::{
    Credential, Fetch, Fetched, Secret, SecretCache, SourceError, SourceSpec, Tool,
};
use puddle_store::RepoToggles;
use puddle_store::WorkspaceGit;
use puddle_types::{Event, GitAccess, Host, ReasonCode};
use support::{Ask as _, basic};

/// Shortcuts over [`Ask`] for the cases that send no body, and the body the injector asked for.
trait Shortcuts {
    async fn decide(
        &self,
        host: &str,
        method: &str,
        target: &str,
        headers: &[&str],
    ) -> InjectDecision;

    fn body_wanted(&self, host: &str, method: &str, target: &str) -> Option<usize>;
}

impl Shortcuts for World {
    async fn decide(
        &self,
        host: &str,
        method: &str,
        target: &str,
        headers: &[&str],
    ) -> InjectDecision {
        self.decide_with(host, method, target, headers, None).await
    }

    fn body_wanted(&self, host: &str, method: &str, target: &str) -> Option<usize> {
        let host = Host::parse_normalised(host).unwrap();
        let context = InjectContext {
            workspace: &self.workspace,
            host: &host,
        };
        let view = RequestView::new(method, target, &[]);
        self.injector.body_wanted(&context, &view)
    }
}

/// Whether `decision` injects exactly one `authorization` header with the value `expected`.
fn injected(decision: &InjectDecision, expected: &str) -> bool {
    match decision {
        InjectDecision::Inject(injection) => {
            injection.headers().len() == 1
                && injection
                    .headers()
                    .iter()
                    .all(|h| h.name().as_str() == "authorization" && h.value_is(expected))
        }
        _ => false,
    }
}

fn refusal(decision: &InjectDecision) -> (u16, &'static str, String) {
    match decision {
        InjectDecision::Refuse(r) => (r.status(), r.code().as_str(), r.message().to_owned()),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

const FETCH: &str = "/acme/web.git/info/refs?service=git-upload-pack";
const PUSH_ADVERT: &str = "/acme/web.git/info/refs?service=git-receive-pack";
const OWN: &str = "authorization: Bearer the-workspaces-own-token";
const STAND_IN: &str =
    "authorization: Basic cHVkZGxlLXNlY3JldC1GT08tMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAw";

fn token_header(token: &str) -> String {
    basic("x-access-token", token)
}

/// A workspace with Work (owns `acme`), Personal (the rest of github.com) and the workspace's own
/// repository `github.com/acme/web` listed with Pull and Push.
fn world() -> World {
    let w = World::new();
    w.identity("Work", "github.com", &["acme"], false, "CANARY-WORK");
    w.identity("Personal", "github.com", &[], true, "CANARY-PERSONAL");
    w.list("github.com/acme/web", RepoToggles::BOTH);
    w
}

mod azure;
mod identity;
mod lfs;
mod notices;
mod paths;
mod repository_table;
mod sources;
mod store_settings;
