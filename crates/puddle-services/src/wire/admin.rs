// SPDX-License-Identifier: AGPL-3.0-or-later
//! The admin screen on the wire: the user list with counts, the server's totals, the audit of what
//! the owner did to accounts, and the one-time invite. Only [`InviteIssued`] carries a secret (the
//! invite link's, shown once to the owner who asked for it); every other type is counts, dates and
//! names, and a test keeps it that way: no password, hash, device key, session or credential-store
//! content, and no workspace name, request, rule or audit line of a user, is a field of any of them.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Where an account stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AdminUserState {
    /// Made by the owner; the person has not chosen a password yet.
    Invited,
    /// The person has a password.
    Active,
    /// The owner is deleting the account; it can't sign in and is removed once its workspaces
    /// are gone.
    Deleting,
}

/// One account as the owner sees it: counts and sizes, never what is inside.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AdminUser {
    /// The account's id.
    pub id: i64,
    /// The name.
    pub name: String,
    /// `owner` or `user`.
    pub role: String,
    /// Invited, active or being deleted.
    pub state: AdminUserState,
    /// When the account was made, epoch ms.
    pub created_at: u64,
    /// When they last signed in, epoch ms; `null` if they never have.
    #[schema(required = true)]
    pub last_sign_in: Option<u64>,
    /// Browsers signed in as them that still work.
    pub devices: u32,
    /// Their workspaces that are running or starting.
    pub workspaces_running: u32,
    /// Their workspaces that are not running.
    pub workspaces_stopped: u32,
    /// What their workspaces' disks hold, in MiB; workspaces whose use is unknown count nothing.
    pub disk_used_mib: u64,
}

/// The accounts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AdminUserList {
    /// The owner first, then by id.
    pub users: Vec<AdminUser>,
}

/// What this Puddle holds in total. There are no per-user limits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AdminStats {
    /// Accounts, the owner included.
    pub users: u32,
    /// Workspaces of every user.
    pub workspaces: u32,
    /// Running or starting workspaces.
    pub running: u32,
    /// What every workspace's disks hold, in MiB.
    pub disk_used_mib: u64,
    /// Memory the running workspaces were given, in MiB.
    pub memory_mib: u64,
    /// Logical processors of this computer.
    pub host_cpus: u32,
}

/// The name of the account to make.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateUserRequest {
    /// The new user's name: 1 to 64 characters, unique ignoring case.
    pub name: String,
}

/// A new invite, shown once: the account and the secret that goes after `#` in the link
/// `<this address>/invite#<secret>`. It works once and for 24 hours, and nothing can read it
/// back afterwards.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct InviteIssued {
    /// The account the invite is for.
    pub user: AdminUser,
    /// The link's secret.
    pub invite_secret: String,
    /// When the invite stops working, epoch ms.
    pub expires_at: u64,
}

/// What the owner did to an account.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AdminActionKind {
    /// An account was made.
    UserCreated,
    /// A new invite was made (an active user's password and sessions ended).
    InviteReissued,
    /// Deleting an account began.
    UserDeleteStarted,
    /// An account and everything of theirs is gone.
    UserDeleted,
}

/// One line of the admin audit: who did what to whom, and when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AdminAuditLine {
    /// When, epoch ms.
    pub at: u64,
    /// The owner who did it.
    pub actor: String,
    /// What was done.
    pub action: AdminActionKind,
    /// The account's id (never reused).
    pub target_id: i64,
    /// The account's name when it happened.
    pub target: String,
}

/// The newest admin audit lines.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AdminAuditList {
    /// Newest first.
    pub lines: Vec<AdminAuditLine>,
}
