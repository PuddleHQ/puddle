// SPDX-License-Identifier: GPL-3.0-or-later
//! GitHub: the repositories a token reaches (`GET /user/repos`, paged), and who it is (`GET /user`,
//! `GET /user/orgs`).
//!
//! `/user/repos` answers what the token can reach: a classic token or a sign-in through an app
//! reaches everything the account does; a fine-grained token lists only the repositories it was
//! granted. Paging follows the `Link` header the way GitHub asks, but only to the same host and
//! path: a next page on another address is refused, because the token goes with the request.

use std::collections::{HashMap, HashSet};

use serde::Deserialize;

use crate::api::{ApiReply, ApiRequest, Authorization};
use crate::exchange::Exchange;
use crate::limits::github_spent;
use crate::model::{
    Listed, Note, NoteKind, Problem, ProblemKind, Profile, Repository, Role, Visibility,
};

const ACCEPT: &str = "application/vnd.github+json";
const HEADERS: &[(&str, &str)] = &[("x-github-api-version", "2022-11-28")];
/// 100 repositories a page, so this reads up to 3000.
const MAX_PAGES: usize = 30;
/// Organisations a profile lists: 5 pages of 100.
const MAX_ORG_PAGES: usize = 5;

/// Where a GitHub host's API is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Endpoint {
    /// The host the API answers on (`api.github.com`).
    pub(crate) api_host: String,
    /// What precedes every path (`/api/v3` on an enterprise server).
    base: &'static str,
    /// The host people clone from (`github.com`), with its port if it has one.
    pub(crate) web_host: String,
}

/// The API of `host`, or `None` when it is not known to be GitHub. `github.com` and `*.ghe.com`
/// are; any other host is when the credential comes from `gh`, which only signs in to GitHub
/// hosts (an enterprise server, whose API is under `/api/v3`).
pub(crate) fn endpoint(host: &str, signed_in_with_gh: bool) -> Option<Endpoint> {
    let web_host = host.to_ascii_lowercase();
    let (api_host, base) = if web_host == "github.com" {
        ("api.github.com".to_owned(), "")
    } else if web_host.ends_with(".ghe.com") && !web_host.starts_with("api.") {
        (format!("api.{web_host}"), "")
    } else if signed_in_with_gh {
        (web_host.clone(), "/api/v3")
    } else {
        return None;
    };
    Some(Endpoint {
        api_host,
        base,
        web_host,
    })
}

fn request(
    endpoint: &Endpoint,
    path: &str,
    authorization: &Authorization,
    if_none_match: Option<String>,
) -> ApiRequest {
    ApiRequest {
        host: endpoint.api_host.clone(),
        path: path.to_owned(),
        accept: ACCEPT,
        headers: HEADERS,
        authorization: authorization.clone(),
        if_none_match,
    }
}

/// What one page of the repository list said.
#[derive(Debug, Clone)]
struct Page {
    repos: Vec<Repository>,
    /// The `Link` header, for the page after this one.
    link: Option<String>,
    sso_partial: bool,
}

/// A [`Page`] with the `ETag` it came with.
#[derive(Debug, Clone)]
struct CachedPage {
    etag: String,
    page: Page,
}

/// The pages of one credential's repository list as last read, by the path they were asked at.
/// A page asked again with its `ETag` that the host says is unchanged (`304`) is not counted
/// against the host's rate limit and costs no body, so the copy kept here stands in for it.
#[derive(Debug, Default)]
pub(crate) struct Pages(HashMap<String, CachedPage>);

#[derive(Deserialize)]
struct RepoJson {
    full_name: String,
    private: Option<bool>,
    visibility: Option<String>,
    archived: Option<bool>,
    fork: Option<bool>,
    permissions: Option<Permissions>,
}

#[derive(Deserialize, Default)]
struct Permissions {
    admin: Option<bool>,
    maintain: Option<bool>,
    push: Option<bool>,
    triage: Option<bool>,
    pull: Option<bool>,
}

impl Permissions {
    fn role(&self) -> Role {
        let has = |p: Option<bool>| p == Some(true);
        if has(self.admin) {
            Role::Admin
        } else if has(self.maintain) {
            Role::Maintain
        } else if has(self.push) {
            Role::Write
        } else if has(self.triage) {
            Role::Triage
        } else if has(self.pull) {
            Role::Read
        } else {
            Role::Unknown
        }
    }
}

/// A user or repository name: what GitHub allows, and nothing that could change an address.
fn name_ok(segment: &str) -> bool {
    !segment.is_empty()
        && segment.len() <= 100
        && segment != "."
        && segment != ".."
        && segment
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

fn repository(json: RepoJson, endpoint: &Endpoint) -> Result<Repository, Problem> {
    let (owner, name) = json
        .full_name
        .split_once('/')
        .filter(|(owner, name)| name_ok(owner) && name_ok(name))
        .ok_or_else(|| Problem::bad_answer("a repository with a name GitHub does not use"))?;
    let visibility = match (json.visibility.as_deref(), json.private) {
        (Some("public"), _) | (None, Some(false)) => Visibility::Public,
        (Some("private"), _) | (None, Some(true)) => Visibility::Private,
        (Some("internal"), _) => Visibility::Internal,
        _ => Visibility::Unknown,
    };
    Ok(Repository {
        host: endpoint.web_host.clone(),
        owner: owner.to_owned(),
        project: None,
        name: name.to_owned(),
        full_name: json.full_name.clone(),
        url: format!("https://{}/{}", endpoint.web_host, json.full_name),
        visibility,
        role: json.permissions.unwrap_or_default().role(),
        archived: json.archived == Some(true),
        fork: json.fork == Some(true),
    })
}

/// The path of the next page from a `Link` header, only when it stays on this host and under
/// `listing` (the path this read started with, without its query).
fn next_page(
    link: Option<&str>,
    endpoint: &Endpoint,
    listing: &str,
) -> Result<Option<String>, Problem> {
    let Some(next) = link.and_then(next_target) else {
        return Ok(None);
    };
    let expected = format!("https://{}{}{}", endpoint.api_host, endpoint.base, listing);
    let own = next
        .get(..expected.len())
        .filter(|head| head.eq_ignore_ascii_case(&expected))
        .and_then(|_| next.get(expected.len()..))
        .filter(|rest| rest.is_empty() || rest.starts_with('?'));
    match own {
        Some(rest) => Ok(Some(format!("{}{}{}", endpoint.base, listing, rest))),
        None => Err(Problem::bad_answer(
            "a next page on another address, which puddle does not follow",
        )),
    }
}

/// The address a `Link` header gives for `rel="next"`.
fn next_target(link: &str) -> Option<&str> {
    link.split(',').find_map(|part| {
        let (target, params) = part.split_once(';')?;
        let is_next = params.split(';').any(|p| {
            let p = p.trim();
            p.eq_ignore_ascii_case("rel=\"next\"") || p.eq_ignore_ascii_case("rel=next")
        });
        let target = target.trim().strip_prefix('<')?.strip_suffix('>')?;
        is_next.then_some(target)
    })
}

/// The page `reply` answers for `path`: read from the body, or, for a `304`, what `pages` kept.
fn read_page(
    reply: &ApiReply,
    endpoint: &Endpoint,
    path: &str,
    pages: &mut Pages,
) -> Result<Page, Problem> {
    if reply.status == 304 {
        return pages
            .0
            .get(path)
            .map(|kept| kept.page.clone())
            .ok_or_else(|| {
                Problem::bad_answer(
                    "GitHub said a page is unchanged that puddle had not asked about",
                )
            });
    }
    let rows: Vec<RepoJson> = serde_json::from_slice(&reply.body).map_err(|_| {
        Problem::bad_answer("the list of repositories is not what GitHub documents")
    })?;
    let repos = rows
        .into_iter()
        .map(|row| repository(row, endpoint))
        .collect::<Result<Vec<_>, _>>()?;
    let page = Page {
        repos,
        link: reply.header("link").map(str::to_owned),
        sso_partial: reply
            .header("x-github-sso")
            .is_some_and(|v| v.trim_start().starts_with("partial-results")),
    };
    match reply.header("etag") {
        Some(etag) => {
            pages.0.insert(
                path.to_owned(),
                CachedPage {
                    etag: etag.to_owned(),
                    page: page.clone(),
                },
            );
        }
        None => {
            pages.0.remove(path);
        }
    }
    Ok(page)
}

/// Every repository the token reaches, in name order, up to 3000. Each page is asked with the
/// `ETag` `pages` kept for it.
pub(crate) async fn list(
    exchange: &Exchange<'_>,
    endpoint: &Endpoint,
    authorization: &Authorization,
    pages: &mut Pages,
) -> Result<Listed, Problem> {
    const LISTING: &str = "/user/repos";
    let mut path = format!(
        "{}{LISTING}?per_page=100&sort=full_name&direction=asc",
        endpoint.base
    );
    let mut repos = Vec::new();
    let mut notes = Vec::new();
    let mut sso_partial = false;
    let mut asked = HashSet::new();
    let mut page = 1;
    let spent_until = loop {
        let etag = pages.0.get(&path).map(|kept| kept.etag.clone());
        let reply = exchange
            .get(request(endpoint, &path, authorization, etag))
            .await?;
        let read = read_page(&reply, endpoint, &path, pages)?;
        asked.insert(path.clone());
        repos.extend(read.repos);
        sso_partial |= read.sso_partial;
        let Some(next) = next_page(read.link.as_deref(), endpoint, LISTING)? else {
            break github_spent(&reply);
        };
        if page == MAX_PAGES {
            notes.push(Note::new(
                NoteKind::Truncated,
                format!(
                    "puddle reads the first {} repositories and there are more: enter the address of \
                     one that is missing",
                    MAX_PAGES * 100
                ),
            ));
            break github_spent(&reply);
        }
        page += 1;
        path = next;
    };
    // Pages the list no longer has (it got shorter) are not asked about again.
    pages.0.retain(|kept, _| asked.contains(kept));
    if sso_partial {
        notes.push(Note::new(
            NoteKind::SsoPartial,
            "some organisations are missing: they require single sign-on, which this token has not \
             been approved for; approve it for them on GitHub to list their repositories",
        ));
    }
    if authorization.token_starts_with("github_pat_") {
        notes.push(Note::new(
            NoteKind::FineGrainedToken,
            "a fine-grained token lists only the repositories it was granted",
        ));
    }
    Ok(Listed {
        repos,
        notes,
        spent_until,
    })
}

#[derive(Deserialize)]
struct UserJson {
    login: String,
    id: u64,
    name: Option<String>,
}

#[derive(Deserialize)]
struct OrgJson {
    login: String,
}

/// Who the token is, for an identity's author, and the organisations to offer as coverage. The
/// address is GitHub's private no-reply one, so a commit never shows a real address.
pub(crate) async fn profile(
    exchange: &Exchange<'_>,
    endpoint: &Endpoint,
    authorization: &Authorization,
) -> Result<Profile, Problem> {
    let reply = exchange
        .get(request(
            endpoint,
            &format!("{}/user", endpoint.base),
            authorization,
            None,
        ))
        .await?;
    let user: UserJson = serde_json::from_slice(&reply.body)
        .map_err(|_| Problem::bad_answer("the account is not what GitHub documents"))?;
    let host_name = endpoint
        .web_host
        .split_once(':')
        .map_or(endpoint.web_host.as_str(), |(name, _)| name);
    let mut profile = Profile {
        email: Some(format!(
            "{}+{}@users.noreply.{host_name}",
            user.id, user.login
        )),
        name: Some(
            user.name
                .filter(|n| !n.trim().is_empty())
                .unwrap_or_else(|| user.login.clone()),
        ),
        account: Some(user.login),
        ..Profile::default()
    };
    match organisations(exchange, endpoint, authorization).await {
        Ok(organisations) => profile.organisations = organisations,
        Err(problem) if problem.kind == ProblemKind::RateLimited => return Err(problem),
        Err(problem) => profile.notes.push(Note::new(
            NoteKind::OrganisationsUnavailable,
            format!("the organisations could not be listed: {}", problem.message),
        )),
    }
    Ok(profile)
}

async fn organisations(
    exchange: &Exchange<'_>,
    endpoint: &Endpoint,
    authorization: &Authorization,
) -> Result<Vec<String>, Problem> {
    const LISTING: &str = "/user/orgs";
    let mut path = format!("{}{LISTING}?per_page=100", endpoint.base);
    let mut logins = Vec::new();
    for page in 1..=MAX_ORG_PAGES {
        let reply = exchange
            .get(request(endpoint, &path, authorization, None))
            .await?;
        let rows: Vec<OrgJson> = serde_json::from_slice(&reply.body)
            .map_err(|_| Problem::bad_answer("the organisations are not what GitHub documents"))?;
        logins.extend(
            rows.into_iter()
                .map(|org| org.login)
                .filter(|login| name_ok(login)),
        );
        match next_page(reply.header("link"), endpoint, LISTING)? {
            Some(next) if page < MAX_ORG_PAGES => path = next,
            _ => break,
        }
    }
    Ok(logins)
}
