// SPDX-License-Identifier: GPL-3.0-or-later
//! The logins Puddle kept for a workspace, on the wire: which services, since when, never a value.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// A service Puddle keeps a login for in a workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct KeptLoginView {
    /// The service as forgetting names it: `claude` or `github`.
    pub service: String,
    /// What the user calls it: `Claude Code`, `GitHub`.
    pub name: String,
    /// When Puddle first noted the login as kept, epoch ms; `null` when it does not know (the
    /// login was kept before Puddle wrote times down).
    #[schema(required = true)]
    pub since: Option<u64>,
}

/// The logins kept for a workspace. No token, stand-in or account detail is in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct KeptLoginList {
    /// The services with a login kept; empty when none is.
    pub logins: Vec<KeptLoginView>,
    /// `false` when the operating system's credential store did not answer and Puddle had no
    /// record of its own: `logins` is then empty because it is not known, not because nothing is
    /// kept.
    pub store_readable: bool,
}
