// SPDX-License-Identifier: GPL-3.0-or-later
use std::time::SystemTime;

use puddle_api::MemorySettings;
use puddle_repos::{ApiReply, FakeApi, Freshness, Read, Secrets};
use puddle_secrets::{Credential, Secret, SourceError, SourceSpec};
use puddle_settings::WorkspaceSettings;
use puddle_types::LocalCategory;
use serde_json::json;

use super::*;

fn name() -> WorkspaceId {
    WorkspaceId::new("acme").unwrap()
}

#[test]
fn local_access_follows_the_settings_when_they_can_be_read() {
    let repo = MemorySettings::default();
    let mut own = WorkspaceSettings::default();
    own.overrides.local_toggles.private = Some(true);
    repo.save_workspace(&name(), own.to_document()).unwrap();
    let access = local_access(&repo, &name());
    assert!(access.is_on(LocalCategory::Private));
    assert!(!access.is_on(LocalCategory::Loopback));
}

#[test]
fn damaged_settings_block_every_local_destination_and_say_why() {
    let repo = MemorySettings::default();
    repo.save_global(json!("not an object")).unwrap();
    assert_eq!(local_access(&repo, &name()), LocalAccess::UNREADABLE);
    let reason = local_access_checked(&repo, &name())
        .unwrap_err()
        .to_string();
    assert!(reason.contains("global settings are not valid"), "{reason}");

    let repo = MemorySettings::default();
    repo.save_workspace(&name(), json!([1])).unwrap();
    assert_eq!(local_access(&repo, &name()), LocalAccess::UNREADABLE);
    let reason = local_access_checked(&repo, &name())
        .unwrap_err()
        .to_string();
    assert!(reason.contains("workspace acme"), "{reason}");
}

struct OneToken;

impl Secrets for OneToken {
    fn get<'a>(
        &'a self,
        _spec: &'a SourceSpec,
    ) -> futures_util::future::BoxFuture<'a, Result<Credential, SourceError>> {
        Box::pin(async {
            Ok(Credential {
                username: None,
                secret: Arc::new(Secret::new("CANARY-host-token".to_owned())),
            })
        })
    }

    fn invalidate(&self, _spec: &SourceSpec) {}
}

#[tokio::test]
async fn a_repository_list_renewed_behind_the_callers_back_is_announced_as_an_event() {
    use std::collections::BTreeSet;

    use puddle_store::{Coverage, CredentialBinding, IdentityDraft, Limits, ManualClock, Store};

    let host = puddle_secrets::HostName::new("github.com").unwrap();
    let source = SourceSpec::Gh {
        host: host.clone(),
        account: puddle_secrets::AccountName::new("octocat").unwrap(),
    };
    let clock = Arc::new(ManualClock::new(1_700_000_000_000));
    let store = Store::open_in_memory(clock.clone(), Limits::default()).unwrap();
    let identity = store
        .create_identity(
            puddle_store::UserId::OWNER,
            IdentityDraft {
                label: "Me".to_owned(),
                author: puddle_store::Author::new("Me", "me@example.org").unwrap(),
                credentials: vec![
                    CredentialBinding::new(
                        &host,
                        source,
                        Coverage::new(BTreeSet::new(), true).unwrap(),
                    )
                    .unwrap(),
                ],
            },
        )
        .unwrap();
    // The token source answers the same for as long as it is asked.
    OneToken.invalidate(&SourceSpec::Gh {
        host: host.clone(),
        account: puddle_secrets::AccountName::new("octocat").unwrap(),
    });
    let api = Arc::new(FakeApi::new());
    let path = "/user/repos?per_page=100&sort=full_name&direction=asc";
    api.reply("api.github.com", path, ApiReply::new(200, "[]"));
    let repos = Repos::new(api.clone(), Arc::new(OneToken), clock.clone());
    let hub = Arc::new(EventHub::default());
    let mut stream = hub.subscribe();
    announce_renewals(&repos, hub.clone());
    let read = Read {
        only: None,
        freshness: Freshness::Cached,
    };
    repos.lists(std::slice::from_ref(&identity), read).await;

    // A repository appears; the next ask after the list went stale is answered with the old
    // list and the new one is announced.
    api.clear("api.github.com", path);
    api.reply(
            "api.github.com",
            path,
            ApiReply::new(200, r#"[{"id":1,"name":"new","full_name":"octo/new","private":false,"html_url":"https://github.com/octo/new"}]"#),
        );
    clock.advance(11 * 60_000);
    let served = repos.lists(std::slice::from_ref(&identity), read).await;

    assert!(served[0].repos.is_empty());
    let event = tokio::time::timeout(std::time::Duration::from_secs(5), stream.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(event, Event::RepoListsChanged {});
}

#[test]
fn the_distribution_points_come_from_the_synced_roots_and_only_from_them() {
    use puddle_certs::{SOURCES, StoreSnapshot};

    let root = |cn: &str, url: &str| {
        let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, cn);
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        params.not_after = rcgen::date_time_ymd(2090, 1, 1);
        params.crl_distribution_points = vec![rcgen::CrlDistributionPoint {
            uris: vec![url.to_owned()],
        }];
        let key = rcgen::KeyPair::generate().unwrap();
        params.self_signed(&key).unwrap().der().to_vec()
    };
    let mut snapshot = StoreSnapshot::new();
    snapshot.add(
        SOURCES[0],
        root("Corp Root", "http://pki.corp.example/root.crl"),
    );
    // Expired: not synced, so its address is not a distribution point either.
    let mut old = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
    old.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    old.not_after = rcgen::date_time_ymd(2001, 1, 1);
    old.crl_distribution_points = vec![rcgen::CrlDistributionPoint {
        uris: vec!["http://old.corp.example/x.crl".to_owned()],
    }];
    let key = rcgen::KeyPair::generate().unwrap();
    snapshot.add(SOURCES[0], old.self_signed(&key).unwrap().der().to_vec());
    let roots = CorporateRoots::select(&snapshot, SystemTime::now());

    let points = distribution_points(&roots);
    let host = |n: &str| puddle_types::Host::parse_normalised(n).unwrap();
    assert!(points.covers(&host("pki.corp.example"), 80, "GET", "/root.crl"));
    assert!(!points.covers(&host("old.corp.example"), 80, "GET", "/x.crl"));
    assert!(distribution_points(&CorporateRoots::default()).is_empty());
}
