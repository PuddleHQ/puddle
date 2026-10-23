// SPDX-License-Identifier: GPL-3.0-or-later
//! Wire types of a workspace's checkouts and of adding, managing and removing repositories.

use puddle_store::IdentityId;
use puddle_types::CheckoutDir;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::checkouts as domain;
use crate::error::ApiError;
use crate::wire::WorkspaceRepository;
use crate::workspaces::{NewRepository, RepoUrl, RepositoryDraft, check_repositories};

/// Why a checkout's remote address is not shown as it stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RemoteNote {
    /// The address had a user name or password in it; they were removed before it left the
    /// parser, and `remote_url` is the address without them.
    CredentialsRemoved,
    /// An SSH address ("SSH is not supported yet"); `remote_url` is `null`.
    Ssh,
    /// Not an `https://` address (`http://`, `git://`, a path); `remote_url` is `null`.
    NotHttps,
    /// The checkout has no remote, or several and none called `origin`; `remote_url` is `null`.
    NoRemote,
}

impl From<domain::RemoteNote> for RemoteNote {
    fn from(note: domain::RemoteNote) -> Self {
        match note {
            domain::RemoteNote::CredentialsRemoved => Self::CredentialsRemoved,
            domain::RemoteNote::Ssh => Self::Ssh,
            domain::RemoteNote::NotHttps => Self::NotHttps,
            domain::RemoteNote::NoRemote => Self::NoRemote,
        }
    }
}

/// Whether "Manage this" is offered for a checkout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CheckoutManage {
    /// Whether the checkout can be turned into a list entry.
    pub offered: bool,
    /// Why not, when it cannot (for the user; guest text may be in it).
    #[schema(required = true)]
    pub reason: Option<String>,
}

/// One checkout found directly under the workspace's volume root. Every string except `entry`
/// came from the guest: escape it when rendering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Checkout {
    /// Its directory under the volume root.
    pub directory: String,
    /// The address of its `origin` remote (else of its only remote) when that is an `https://`
    /// address, without any user name or password; `null` otherwise (see `remote_note`).
    #[schema(required = true)]
    pub remote_url: Option<String>,
    /// Why `remote_url` is `null`, or that credentials were removed from it; `null` when
    /// neither.
    #[schema(required = true)]
    pub remote_note: Option<RemoteNote>,
    /// The branch; `null` when HEAD is detached.
    #[schema(required = true)]
    pub branch: Option<String>,
    /// The short commit; `null` before the first commit.
    #[schema(required = true)]
    pub head: Option<String>,
    /// Whether it has uncommitted changes or untracked files.
    pub dirty: bool,
    /// Why Puddle could not read it, when it could not; `null` otherwise.
    #[schema(required = true)]
    pub unreadable: Option<String>,
    /// The list entry with this directory, or `null` when the checkout is not on the list. Its
    /// address may differ from `remote_url`: show both then.
    #[schema(required = true)]
    pub entry: Option<WorkspaceRepository>,
    /// Whether "Manage this" is offered.
    pub manage: CheckoutManage,
}

/// `GET /api/workspaces/{id}/checkouts`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Checkouts {
    /// Whether the workspace is running: `checkouts` is then from the moment of the call.
    pub running: bool,
    /// When the listing was made (epoch ms); `null` before the first one this run.
    #[schema(required = true)]
    pub checked_at: Option<u64>,
    /// Why there is no listing (the workspace is stopped, or the listing failed or its output
    /// was refused); `null` when there is one.
    #[schema(required = true)]
    pub problem: Option<String>,
    /// How many checkouts there are past the first 100.
    pub more: u64,
    /// The checkouts found, in directory order.
    pub checkouts: Vec<Checkout>,
    /// The directories of list entries that are not on the volume ("not on the disk"). Empty
    /// when the listing stopped at its limit: an entry past it may be there.
    pub missing: Vec<CheckoutDir>,
}

impl From<domain::Checkouts> for Checkouts {
    fn from(c: domain::Checkouts) -> Self {
        Self {
            running: c.running,
            checked_at: c.checked_at,
            problem: c.problem,
            more: c.more,
            checkouts: c
                .checkouts
                .into_iter()
                .map(|c| Checkout {
                    directory: c.found.directory,
                    remote_url: c.found.remote_url,
                    remote_note: c.found.remote_note.map(Into::into),
                    branch: c.found.branch,
                    head: c.found.head,
                    dirty: c.found.dirty,
                    unreadable: c.found.unreadable,
                    entry: c.entry.map(Into::into),
                    manage: CheckoutManage {
                        offered: c.manage.offered,
                        reason: c.manage.reason,
                    },
                })
                .collect(),
            missing: c.missing,
        }
    }
}

/// `POST /api/workspaces/{id}/repositories`: what to add.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AddRepositoryRequest {
    /// Clone a new repository into the running workspace.
    Clone {
        /// The repository, as an `https://` URL without credentials.
        url: String,
        /// The checkout's directory under the volume root; left out or `null` is the last part
        /// of the address. It must not exist yet.
        #[serde(default)]
        directory: Option<String>,
        /// The identity that serves it; left out or `null` is the identity that covers its
        /// owner, else the default identity.
        #[serde(default)]
        identity_id: Option<i64>,
        /// Pull on its new repository table row; left out or `null` is on.
        #[serde(default)]
        pull: Option<bool>,
        /// Push on its new repository table row; left out or `null` is on.
        #[serde(default)]
        push: Option<bool>,
    },
    /// Manage a checkout that is already on the volume ("Manage this"). Puddle reads its remote
    /// again at that moment and changes nothing in the checkout.
    Existing {
        /// The checkout's directory under the volume root.
        directory: String,
        /// The address the checkout list showed for it. Puddle reads the remote again and refuses
        /// with 409 when it names another repository.
        url: String,
        /// As for a clone.
        #[serde(default)]
        identity_id: Option<i64>,
        /// As for a clone.
        #[serde(default)]
        pull: Option<bool>,
        /// As for a clone.
        #[serde(default)]
        push: Option<bool>,
    },
}

impl AddRepositoryRequest {
    /// The checked request; `identity_exists` says whether an identity id names one.
    pub(crate) fn into_add(
        self,
        identity_exists: impl Fn(IdentityId) -> bool,
    ) -> Result<domain::AddRepository, ApiError> {
        match self {
            Self::Clone {
                url,
                directory,
                identity_id,
                pull,
                push,
            } => {
                let draft = RepositoryDraft {
                    url,
                    directory,
                    identity: identity_id.map(IdentityId),
                    pull,
                    push,
                };
                let mut checked =
                    check_repositories(&[draft], identity_exists).map_err(ApiError::invalid)?;
                let new: NewRepository = checked
                    .pop()
                    .ok_or_else(|| ApiError::invalid("the repository is missing"))?;
                Ok(domain::AddRepository::Clone(new))
            }
            Self::Existing {
                directory,
                url,
                identity_id,
                pull,
                push,
            } => {
                let directory = CheckoutDir::new(&directory).map_err(|e| {
                    ApiError::invalid(format!("directory {directory:?}: {}", e.reason()))
                })?;
                let seen = RepoUrl::parse(&url)
                    .map_err(|e| ApiError::invalid(format!("url {url:?}: {e}")))?;
                let identity = identity_id.map(IdentityId);
                if let Some(id) = identity
                    && !identity_exists(id)
                {
                    return Err(ApiError::invalid(format!("there is no identity {}", id.0)));
                }
                Ok(domain::AddRepository::Existing {
                    directory,
                    seen,
                    identity,
                    pull: pull.unwrap_or(true),
                    push: push.unwrap_or(true),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_note_has_its_wire_name() {
        for (note, wire) in [
            (
                domain::RemoteNote::CredentialsRemoved,
                "credentials_removed",
            ),
            (domain::RemoteNote::Ssh, "ssh"),
            (domain::RemoteNote::NotHttps, "not_https"),
            (domain::RemoteNote::NoRemote, "no_remote"),
        ] {
            assert_eq!(serde_json::to_value(RemoteNote::from(note)).unwrap(), wire);
        }
    }
}
