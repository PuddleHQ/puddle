// SPDX-License-Identifier: GPL-3.0-or-later
//! The fake Git hosts' repository lists: [`FakeRepos`] with a gate, so a test can keep a list
//! slow (the request stays open) and then let it go, and the seed types turned into the lists the
//! real service would answer with.

use futures_util::future::BoxFuture;
use puddle_api::wire::{RepoListState, RepoNoteCode, RepoProblemCode, RepoRole, RepoVisibility};
use puddle_api::{FakeCredentials, FakeRepos, RepoService, ReposError};
use puddle_repos::{
    ListState, Note, NoteKind, Problem, ProblemKind, Profile, ProfileRead, Read, Repository, Role,
    SourceList, Visibility,
};
use puddle_secrets::SourceSpec;
use puddle_store::{Identity, IdentityId};
use std::sync::{Arc, Mutex, PoisonError};

use tokio::sync::watch;

use super::scenario::{ProfileSeed, RepoItemSeed, RepoListSeed};

/// A [`FakeRepos`] whose answers wait while it is held.
pub(super) struct HeldRepos {
    inner: FakeRepos,
    held: watch::Sender<bool>,
    credentials: Arc<FakeCredentials>,
    /// What the host says about each pasted token, by the token's value.
    by_token: Mutex<Vec<(String, ProfileRead)>>,
}

impl HeldRepos {
    /// `credentials` is where pasted tokens are kept; a source naming one is answered by the
    /// token's value.
    pub(super) fn new(credentials: Arc<FakeCredentials>) -> Self {
        Self {
            inner: FakeRepos::new(),
            held: watch::channel(false).0,
            credentials,
            by_token: Mutex::new(Vec::new()),
        }
    }

    pub(super) fn set_lists(&self, lists: Vec<SourceList>) {
        self.inner.set_lists(lists);
    }

    pub(super) fn set_profiles(&self, profiles: &[ProfileSeed]) -> Result<(), String> {
        let mut by_token = self.by_token.lock().unwrap_or_else(PoisonError::into_inner);
        by_token.clear();
        for seed in profiles {
            let read = profile_read(seed);
            match (&seed.source, &seed.token) {
                (Some(source), None) => self.inner.set_profile(source.clone().into_spec()?, read),
                (None, Some(token)) => by_token.push((token.clone(), read)),
                _ => {
                    return Err("a profile seed names a `source` or a `token`, not both".to_owned());
                }
            }
        }
        Ok(())
    }

    /// Keeps every answer from now on waiting.
    pub(super) fn hold(&self) {
        self.held.send_replace(true);
    }

    /// Lets the waiting answers go.
    pub(super) fn release(&self) {
        self.held.send_replace(false);
    }
}

impl RepoService for HeldRepos {
    fn lists<'a>(
        &'a self,
        identities: &'a [Identity],
        read: Read,
    ) -> BoxFuture<'a, Result<Vec<SourceList>, ReposError>> {
        Box::pin(async move {
            let mut gate = self.held.subscribe();
            // The sender lives as long as `self`, so the wait ends only when released.
            drop(gate.wait_for(|held| !*held).await);
            self.inner.lists(identities, read).await
        })
    }

    fn profile(&self, source: SourceSpec) -> BoxFuture<'_, Result<ProfileRead, ReposError>> {
        let seeded = match &source {
            SourceSpec::Stored { id, .. } => {
                self.credentials.token_value(id.as_str()).and_then(|value| {
                    self.by_token
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .iter()
                        .find(|(token, _)| *token == value)
                        .map(|(_, read)| read.clone())
                })
            }
            _ => None,
        };
        match seeded {
            Some(read) => Box::pin(async move { Ok(read) }),
            None => self.inner.profile(source),
        }
    }
}

/// The lists the seeds describe, each for the identity its label names.
pub(super) fn lists(
    seeds: &[RepoListSeed],
    identities: &[(String, IdentityId)],
    now: u64,
) -> Result<Vec<SourceList>, String> {
    seeds
        .iter()
        .map(|seed| {
            let identity = identities
                .iter()
                .find(|(label, _)| *label == seed.identity)
                .map(|(_, id)| *id)
                .ok_or_else(|| format!("repository list: no identity {:?}", seed.identity))?;
            Ok(SourceList {
                identity,
                credential: seed.credential,
                host: seed.host.clone(),
                organisation: seed.organisation.clone(),
                state: state(seed.state),
                refreshed_at: seed.refreshed_ago_ms.map(|ago| now.saturating_sub(ago)),
                retry_at: seed.retry_in_ms.map(|ms| now + ms),
                problem: seed.problem.as_ref().map(|p| Problem {
                    kind: problem(p.code),
                    message: p.message.clone(),
                    retry_at: seed.retry_in_ms.map(|ms| now + ms),
                    needs_sign_in: p.needs_sign_in,
                }),
                notes: seed
                    .notes
                    .iter()
                    .map(|n| Note {
                        kind: note(n.code),
                        message: n.message.clone(),
                    })
                    .collect(),
                repos: seed
                    .repos
                    .iter()
                    .map(|item| repository(&seed.host, item))
                    .collect(),
            })
        })
        .collect()
}

fn repository(host: &str, item: &RepoItemSeed) -> Repository {
    let full_name = item.project.as_ref().map_or_else(
        || format!("{}/{}", item.owner, item.name),
        |project| format!("{}/{project}/{}", item.owner, item.name),
    );
    let url = item.url.clone().unwrap_or_else(|| match &item.project {
        Some(project) => format!(
            "https://{host}/{}/{}/_git/{}",
            item.owner,
            project.replace(' ', "%20"),
            item.name
        ),
        None => format!("https://{host}/{}/{}", item.owner, item.name),
    });
    Repository {
        host: host.to_owned(),
        owner: item.owner.clone(),
        project: item.project.clone(),
        name: item.name.clone(),
        full_name,
        url,
        visibility: match item.visibility {
            RepoVisibility::Public => Visibility::Public,
            RepoVisibility::Private => Visibility::Private,
            RepoVisibility::Internal => Visibility::Internal,
            RepoVisibility::Unknown => Visibility::Unknown,
        },
        role: match item.role {
            RepoRole::Admin => Role::Admin,
            RepoRole::Maintain => Role::Maintain,
            RepoRole::Write => Role::Write,
            RepoRole::Triage => Role::Triage,
            RepoRole::Read => Role::Read,
            RepoRole::Unknown => Role::Unknown,
        },
        archived: item.archived,
        fork: item.fork,
    }
}

fn state(state: RepoListState) -> ListState {
    match state {
        RepoListState::Ok => ListState::Ok,
        RepoListState::Stale => ListState::Stale,
        RepoListState::Failed => ListState::Failed,
        RepoListState::Unavailable => ListState::Unavailable,
    }
}

fn problem(code: RepoProblemCode) -> ProblemKind {
    match code {
        RepoProblemCode::NotSignedIn => ProblemKind::NotSignedIn,
        RepoProblemCode::SourceUnavailable => ProblemKind::SourceUnavailable,
        RepoProblemCode::TokenRejected => ProblemKind::TokenRejected,
        RepoProblemCode::Forbidden => ProblemKind::Forbidden,
        RepoProblemCode::NotFound => ProblemKind::NotFound,
        RepoProblemCode::RateLimited => ProblemKind::RateLimited,
        RepoProblemCode::Unreachable => ProblemKind::Unreachable,
        RepoProblemCode::BadAnswer => ProblemKind::BadAnswer,
        RepoProblemCode::Unsupported => ProblemKind::Unsupported,
        RepoProblemCode::OrganisationNeeded => ProblemKind::OrganisationNeeded,
        RepoProblemCode::WrongTarget => ProblemKind::WrongTarget,
    }
}

fn note(code: RepoNoteCode) -> NoteKind {
    match code {
        RepoNoteCode::SsoPartial => NoteKind::SsoPartial,
        RepoNoteCode::OrganisationsMayBeHidden => NoteKind::OrganisationsMayBeHidden,
        RepoNoteCode::FineGrainedToken => NoteKind::FineGrainedToken,
        RepoNoteCode::Truncated => NoteKind::Truncated,
        RepoNoteCode::AuthorUnavailable => NoteKind::AuthorUnavailable,
        RepoNoteCode::OrganisationsUnavailable => NoteKind::OrganisationsUnavailable,
        RepoNoteCode::HostLimitReached => NoteKind::HostLimitReached,
    }
}

fn profile_read(seed: &ProfileSeed) -> ProfileRead {
    ProfileRead {
        profile: Profile {
            account: seed.account.clone(),
            name: seed.name.clone(),
            email: seed.email.clone(),
            organisations: seed.organisations.clone(),
            notes: seed
                .notes
                .iter()
                .map(|n| Note {
                    kind: note(n.code),
                    message: n.message.clone(),
                })
                .collect(),
        },
        problem: seed.problem.as_ref().map(|p| Problem {
            kind: problem(p.code),
            message: p.message.clone(),
            retry_at: None,
            needs_sign_in: p.needs_sign_in,
        }),
    }
}
