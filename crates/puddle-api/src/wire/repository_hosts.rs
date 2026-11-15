// SPDX-License-Identifier: AGPL-3.0-or-later
//! The hosts a workspace create allows for its repositories (storage ST-44a).

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// `POST /api/repository-hosts`: the repositories of a create form, or of a repository to add.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RepositoryHostsRequest {
    /// The repository addresses. One that is not an HTTPS address of a repository has no host
    /// here; the create refuses it by itself.
    pub urls: Vec<String>,
    /// The workspace the repositories are for; leave it out for one that does not exist yet.
    #[serde(default)]
    pub workspace: Option<String>,
}

/// Where a repository host stands with the rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryHostState {
    /// No rule decides it: the create allows it for the workspace.
    New,
    /// A rule or a rule set allows it already; the create adds nothing.
    Allowed,
    /// A block rule or a blocking rule set decides it; the create refuses and never overrides it.
    Blocked,
}

/// One host of the repositories.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RepositoryHost {
    /// The host, lower case.
    pub host: String,
    /// Where it stands.
    pub state: RepositoryHostState,
    /// For `blocked`: which rule or rule set blocks it, in words; `null` otherwise.
    #[schema(required = true)]
    pub blocked_by: Option<String>,
}

/// The hosts of the addresses, in the order they first appear, each once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RepositoryHosts {
    /// One entry per host.
    pub hosts: Vec<RepositoryHost>,
}

/// A host a create (or adding a repository) allowed for a workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AllowedHost {
    /// The host.
    pub host: String,
    /// The rule that allows it, visible on the Rules screen as made by the create.
    pub rule_id: i64,
}
