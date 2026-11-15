// SPDX-License-Identifier: AGPL-3.0-or-later
//! What the owner's admin screen and `puddle user` read about the people on this Puddle: counts
//! and dates, and the audit of what the owner did to accounts. No type here carries a credential,
//! a workspace name or anything a user keeps; the credentials live behind [`crate::AccountSecrets`].

use crate::users::{User, UserId};

/// The most audit lines one read returns.
pub const MAX_ADMIN_AUDIT_LINES: u32 = 500;

/// One user as the admin list sees them, before the workspace counts (which the workspace
/// service knows) are added.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserFacts {
    /// The user.
    pub user: User,
    /// When the owner started deleting them, epoch ms; `None` for a user who is not being
    /// deleted.
    pub deleting_since: Option<u64>,
    /// Browsers signed in as them that still work.
    pub devices: u32,
    /// When they last signed in, epoch ms; `None` if they never have.
    pub last_sign_in: Option<u64>,
}

/// What the owner did to an account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminAction {
    /// A user account was made (in the invited state).
    UserCreated,
    /// A new invite link was made; the user's password and sessions, if any, were ended.
    InviteReissued,
    /// Deleting the user began: they can no longer sign in.
    UserDeleteStarted,
    /// The user and everything of theirs is gone.
    UserDeleted,
}

impl AdminAction {
    /// The stored spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UserCreated => "user_created",
            Self::InviteReissued => "invite_reissued",
            Self::UserDeleteStarted => "user_delete_started",
            Self::UserDeleted => "user_deleted",
        }
    }

    pub(crate) fn parse(text: &str) -> Option<Self> {
        match text {
            "user_created" => Some(Self::UserCreated),
            "invite_reissued" => Some(Self::InviteReissued),
            "user_delete_started" => Some(Self::UserDeleteStarted),
            "user_deleted" => Some(Self::UserDeleted),
            _ => None,
        }
    }
}

/// One line of the admin audit: who did what to whom, and when. Names are as they were then.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminAuditEntry {
    /// The line's id; later lines have higher ids.
    pub id: i64,
    /// When, epoch ms.
    pub at: u64,
    /// Who did it.
    pub actor: UserId,
    /// Their name then.
    pub actor_name: String,
    /// What was done.
    pub action: AdminAction,
    /// To whom (the id is never reused).
    pub target: UserId,
    /// Their name then.
    pub target_name: String,
}

#[cfg(test)]
mod tests;
