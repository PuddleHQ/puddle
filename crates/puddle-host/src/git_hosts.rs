// SPDX-License-Identifier: AGPL-3.0-or-later
//! What the Git hosts are called in URLs: the names Puddle decrypts for a credential, and the
//! remote URLs that belong to an owner (which choose the commit author).
//!
//! Azure DevOps has two names for one thing, `dev.azure.com/{org}` and `{org}.visualstudio.com`,
//! so a credential on `dev.azure.com` covers both.

use puddle_boot::{GitAuthorRule, GitIdentity, MAX_GIT_AUTHOR_GLOBS, MAX_GIT_AUTHOR_RULES};
use std::sync::Arc;

use puddle_proxy::{GitSshHostKeys, TerminationSet, git_ssh_built_in_hosts};
use puddle_store::{Identity, WorkspaceGit};
use puddle_types::{Host, Problem, Problems, WorkspaceId};

/// Azure DevOps's own host; its organisations also answer on `{org}.visualstudio.com`.
const AZURE_DEVOPS: &str = "dev.azure.com";
const AZURE_DEVOPS_LEGACY: &str = "visualstudio.com";

/// The only port Puddle decrypts.
const HTTPS_PORT: &str = "443";

/// The names Puddle decrypts for a credential on `host` (a name, with a port only when the host
/// is not on 443): the host itself, and for Azure DevOps its `*.visualstudio.com` names. A host
/// on another port gets none, because only port 443 is terminated.
pub(crate) fn decrypt_patterns(host: &str) -> Vec<String> {
    let host = host.to_ascii_lowercase();
    let name = match host.split_once(':') {
        None => host.as_str(),
        Some((name, HTTPS_PORT)) => name,
        Some(_) => return Vec::new(),
    };
    let mut patterns = vec![name.to_owned()];
    if name == AZURE_DEVOPS {
        patterns.push(format!("*.{AZURE_DEVOPS_LEGACY}"));
    }
    patterns
}

/// The hosts a workspace decrypts for its attached identities' credentials.
pub(crate) fn decrypt_set(git: &WorkspaceGit) -> TerminationSet {
    let mut set = TerminationSet::new();
    for identity in &git.identities {
        for credential in &identity.credentials {
            for pattern in decrypt_patterns(credential.host.as_str()) {
                // A pattern the set refuses is an address; `never_decrypted` lists it.
                let _ = set.insert(&pattern);
            }
        }
    }
    set
}

/// The hosts of `git`'s credentials whose SSH Puddle ends itself, on top of the built-in Git
/// hosts: the names it decrypts (an enterprise server), with no wildcard.
pub(crate) fn ssh_credential_hosts(git: &WorkspaceGit) -> TerminationSet {
    let mut set = TerminationSet::new();
    for identity in &git.identities {
        for credential in &identity.credentials {
            for pattern in decrypt_patterns(credential.host.as_str()) {
                if !pattern.contains('*') {
                    // `insert` reports only whether the pattern was new
                    let _ = set.insert(&pattern);
                }
            }
        }
    }
    set
}

/// Every name a workspace with `git` may use for Git over SSH: the built-in Git hosts and the
/// hosts of its credentials. These are the names the guest's `ssh_known_hosts` lists the host
/// keys under.
pub(crate) fn ssh_names(git: &WorkspaceGit) -> Vec<Host> {
    let mut names = git_ssh_built_in_hosts();
    names.extend(
        git.identities
            .iter()
            .flat_map(|identity| &identity.credentials)
            .flat_map(|credential| decrypt_patterns(credential.host.as_str()))
            .filter(|pattern| !pattern.contains('*'))
            .filter_map(|pattern| Host::parse_normalised(&pattern).ok()),
    );
    names.sort_by_key(ToString::to_string);
    names.dedup();
    names
}

/// This start's SSH host keys and the names the guest's `ssh_known_hosts` lists them under.
#[derive(Clone)]
pub(crate) struct SshListing {
    pub(crate) keys: Arc<GitSshHostKeys>,
    pub(crate) hosts: Vec<Host>,
}

impl SshListing {
    /// The same keys for the names `git` has now.
    pub(crate) fn for_git(keys: Arc<GitSshHostKeys>, git: &WorkspaceGit) -> Self {
        Self {
            keys,
            hosts: ssh_names(git),
        }
    }

    /// The `known_hosts` lines of the block.
    pub(crate) fn lines(&self) -> Option<String> {
        self.keys.known_hosts(&self.hosts)
    }
}

/// The credential hosts of `git`'s identities that are never decrypted, so their credential is
/// never used: a port other than 443, or an address. The store refuses to save one now, but a
/// database from an earlier version can hold one.
pub(crate) fn never_decrypted(git: &WorkspaceGit) -> Vec<String> {
    let mut hosts: Vec<String> = git
        .identities
        .iter()
        .flat_map(|identity| &identity.credentials)
        .filter(|credential| {
            let mut probe = TerminationSet::new();
            !decrypt_patterns(credential.host.as_str())
                .iter()
                .any(|pattern| probe.insert(pattern).is_ok())
        })
        .map(|credential| credential.host.to_string())
        .collect();
    hosts.sort();
    hosts.dedup();
    hosts
}

/// Every letter as a two-letter class (`a` as `[aA]`): git matches remote globs case-sensitively
/// and a host or an owner is one name whatever its case.
fn fold(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_ascii_alphabetic() {
                format!("[{}{}]", c.to_ascii_lowercase(), c.to_ascii_uppercase())
            } else {
                c.to_string()
            }
        })
        .collect()
}

/// Git globs for the remote URLs of `owner` on `host`, or of every owner on it (`None`): `https`
/// URLs with and without a `user@` in them, and for Azure DevOps the `visualstudio.com` names.
/// Matching is on the URL as git stores it in `remote.<name>.url`.
pub(crate) fn remote_globs(host: &str, owner: Option<&str>) -> Vec<String> {
    let host = host.to_ascii_lowercase();
    let mut authorities = vec![(host.clone(), owner.map(str::to_owned))];
    if host == AZURE_DEVOPS {
        // `{org}.visualstudio.com/{project}/_git/{repo}`: the organisation is in the host name.
        let legacy = owner.map_or_else(
            || format!("*.{AZURE_DEVOPS_LEGACY}"),
            |org| format!("{org}.{AZURE_DEVOPS_LEGACY}"),
        );
        authorities.push((legacy, None));
    }
    let mut globs = Vec::new();
    for (authority, owner) in authorities {
        let authority = match authority.strip_prefix("*.") {
            // Keep the leading `*.` as the glob it is; fold the rest.
            Some(rest) => format!("*.{}", fold(rest)),
            None => fold(&authority),
        };
        let path = owner.map_or_else(
            || "/**".to_owned(),
            |owner| format!("/{}/**", fold(&owner.to_ascii_lowercase())),
        );
        globs.push(format!("https://{authority}{path}"));
        globs.push(format!("https://*@{authority}{path}"));
    }
    globs
}

/// The commit authors of a workspace: the fallback and the rules by remote.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Authors {
    /// The author of a repository no rule matches: the first identity's.
    pub(crate) fallback: Option<GitIdentity>,
    /// Rules in git's order, lowest priority first: the last matching one wins.
    pub(crate) rules: Vec<GitAuthorRule>,
    /// What had to be left out, one sentence each, for the user: an identity whose author git
    /// cannot keep, and rules past what a plan holds.
    pub(crate) left_out: Vec<String>,
}

/// Why an identity gives its commits no author.
#[derive(Debug, thiserror::Error)]
#[error("the identity {label} gives commits no author: {source}")]
struct AuthorRefused {
    /// The identity's label.
    label: String,
    /// What git cannot keep.
    source: puddle_boot::PlanError,
}

/// The author an identity gives its commits, or why the store's text is not something git can
/// keep.
fn author_of(identity: &Identity) -> Result<GitIdentity, AuthorRefused> {
    GitIdentity::new(&identity.author.name, &identity.author.email).map_err(|source| {
        AuthorRefused {
            label: identity.label.clone(),
            source,
        }
    })
}

/// The authors of `git`'s identities.
///
/// The first identity's author is the fallback. A remote on an owner an identity names gets that
/// identity's author, and beats one on the rest of a host, because the credential follows the same
/// rule; among identities that match equally, the first in the workspace's order wins. Git lets
/// the last matching rule win, so the rules are written in the opposite order: the rest of a host
/// first, then named owners, the last identity first in each.
pub(crate) fn authors(git: &WorkspaceGit) -> Authors {
    let mut left_out = Vec::new();
    let usable: Vec<(&Identity, Option<GitIdentity>)> = git
        .identities
        .iter()
        .map(|identity| match author_of(identity) {
            Ok(author) => (identity, Some(author)),
            Err(why) => {
                left_out.push(why.to_string());
                (identity, None)
            }
        })
        .collect();
    let fallback = usable.first().and_then(|(_, author)| author.clone());
    let mut rules = Vec::new();
    for named in [false, true] {
        for (identity, author) in usable.iter().rev() {
            let Some(author) = author else {
                continue;
            };
            let mut globs = Vec::new();
            for credential in &identity.credentials {
                let host = credential.host.as_str();
                if named {
                    for owner in &credential.covers.owners {
                        globs.extend(remote_globs(host, Some(owner.as_str())));
                    }
                } else if credential.covers.rest_of_host {
                    globs.extend(remote_globs(host, None));
                }
            }
            for chunk in globs.chunks(MAX_GIT_AUTHOR_GLOBS) {
                if let Ok(rule) = GitAuthorRule::new(chunk.to_vec(), author.clone()) {
                    rules.push(rule);
                }
            }
        }
    }
    if let Some(extra) = rules
        .len()
        .checked_sub(MAX_GIT_AUTHOR_RULES)
        .filter(|n| *n > 0)
    {
        left_out.push(format!(
            "{extra} of the author rules are left out because a workspace holds at most \
             {MAX_GIT_AUTHOR_RULES}: remotes on the hosts covered as a whole by the identities \
             listed last get the first identity's author"
        ));
        rules.drain(..extra);
    }
    Authors {
        fallback,
        rules,
        left_out,
    }
}

/// The git release that reads `includeIf "hasconfig:remote.*.url:..."`, which the author rules use.
const AUTHOR_RULES_NEED_GIT: (u32, u32) = (2, 36);

/// What the user is told about a workspace's Git settings that Puddle cannot carry out. Each is
/// one problem on the workspace, raised while the cause stands and cleared when it is gone.
#[derive(Debug, Clone, Copy)]
pub(crate) enum GitProblem {
    /// A credential for a host Puddle never decrypts.
    Credentials,
    /// Authors or author rules left out of the plan.
    LeftOut,
    /// A git too old to read the author rules.
    OldGit,
    /// What the workspace decrypts could not be brought up to date.
    DecryptSync,
    /// The authors in the running guest could not be brought up to date.
    AuthorSync,
}

impl GitProblem {
    const ALL: [Self; 5] = [
        Self::Credentials,
        Self::LeftOut,
        Self::OldGit,
        Self::DecryptSync,
        Self::AuthorSync,
    ];

    fn key(self, workspace: &WorkspaceId) -> String {
        let kind = match self {
            Self::Credentials => "git-credentials",
            Self::LeftOut => "git-left-out",
            Self::OldGit => "git-old",
            Self::DecryptSync => "git-decrypt",
            Self::AuthorSync => "git-authors",
        };
        format!("{kind}:{workspace}")
    }

    /// Raises `problem` when there is one, else ends the problem of this kind.
    pub(crate) fn settle(
        self,
        problems: &Problems,
        workspace: &WorkspaceId,
        problem: Option<(String, String)>,
    ) {
        match problem {
            Some((title, detail)) => problems
                .raise(Problem::new(self.key(workspace), title, detail).about(workspace.clone())),
            None => problems.clear(&self.key(workspace)),
        }
    }

    /// Ends every Git problem of `workspace`: it is not running, so none of them stands.
    pub(crate) fn clear_all(problems: &Problems, workspace: &WorkspaceId) {
        for kind in Self::ALL {
            problems.clear(&kind.key(workspace));
        }
    }
}

/// The problem for credentials Puddle never uses (see [`never_decrypted`]).
pub(crate) fn credentials_problem(name: &str, hosts: &[String]) -> Option<(String, String)> {
    if hosts.is_empty() {
        return None;
    }
    Some((
        format!("{name} has credentials Puddle never uses"),
        format!(
            "Puddle adds credentials only to HTTPS on port 443 and only by host name, so git is \
             not given the one for {} and asks for its own. Remove the credential, or replace it \
             with one for the host's name.",
            hosts.join(", ")
        ),
    ))
}

/// The problem for authors or rules left out of `authors`.
pub(crate) fn left_out_problem(name: &str, authors: &Authors) -> Option<(String, String)> {
    if authors.left_out.is_empty() {
        return None;
    }
    Some((
        format!("Some of {name}'s commit authors are not applied"),
        format!(
            "{}. A remote with no rule of its own gets the first identity's author.",
            authors.left_out.join("; ")
        ),
    ))
}

/// The problem for a guest whose git (`version`, from the boot hook) ignores the author rules in
/// `authors`: none when there are no rules to ignore, no git, or one that reads them.
pub(crate) fn old_git_problem(
    name: &str,
    version: Option<(u32, u32)>,
    authors: &Authors,
) -> Option<(String, String)> {
    let (major, minor) = version.filter(|v| *v < AUTHOR_RULES_NEED_GIT)?;
    if authors.rules.is_empty() {
        return None;
    }
    let (need_major, need_minor) = AUTHOR_RULES_NEED_GIT;
    Some((
        format!("git {major}.{minor} in {name} ignores the commit author rules"),
        format!(
            "Puddle picks the commit author by the repository's remote, which needs git \
             {need_major}.{need_minor} or newer. Every commit in {name} uses the first \
             identity's author until git there is updated; the next start checks again."
        ),
    ))
}

#[cfg(test)]
mod tests;
