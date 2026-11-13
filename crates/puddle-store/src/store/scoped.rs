// SPDX-License-Identifier: GPL-3.0-or-later
//! The store as one user sees it.

use puddle_types::{Decision, Host, WorkspaceId};

use super::{NewRequestRule, RequestRule, Store};
use crate::error::StoreError;
use crate::pattern::Pattern;
use crate::rule::{Actor, Effect, NewRule, Rule, Scope};
use crate::users::UserId;

impl Store {
    /// The calls a user makes on their own account: each one names whose rule it writes or whose
    /// rules it reads, so none falls back to the owner when the caller forgets the user.
    #[must_use]
    pub fn as_user(&self, user: UserId) -> UserStore<'_> {
        UserStore { store: self, user }
    }
}

impl Store {
    /// Allows each of `hosts` for `workspace` that no rule of the workspace's user decides yet,
    /// with an exact allow rule made by [`Actor::WorkspaceCreate`] (a create names the hosts of
    /// its repositories, and the clone that follows would otherwise wait in the inbox). A host
    /// that is allowed already, or that a deny decides, gets no rule: a create never overrides a
    /// block. Returns the rules made.
    ///
    /// # Errors
    /// [`StoreError::UnknownWorkspace`] for a workspace with no recorded user, or a database
    /// error; the rules made before it stay.
    pub fn allow_hosts_for_create(
        &self,
        workspace: &WorkspaceId,
        hosts: &[Host],
    ) -> Result<Vec<Rule>, StoreError> {
        let owner = self
            .workspace_owner(workspace)?
            .ok_or_else(|| StoreError::UnknownWorkspace(workspace.clone()))?;
        self.as_user(owner).allow_hosts_for_create(workspace, hosts)
    }
}

/// A [`Store`] seen as one user (see [`Store::as_user`]).
#[derive(Clone, Copy)]
pub struct UserStore<'a> {
    store: &'a Store,
    user: UserId,
}

impl UserStore<'_> {
    /// Creates a rule of this user and closes the open rows it now decides (R-16): a global rule
    /// becomes theirs, and a workspace or set it names must be theirs.
    ///
    /// # Errors
    /// [`StoreError::SystemActor`], [`StoreError::ExpiryNotInFuture`],
    /// [`StoreError::UnknownWorkspace`] or [`StoreError::UnknownRuleSet`] for another user's
    /// workspace or set, or a database error.
    pub fn add_rule(&self, new: &NewRule) -> Result<Rule, StoreError> {
        self.store.add_rule_by(self.user, new)
    }

    /// Makes a rule on method and path of this user: global for their workspaces, or for one of
    /// them, and tells running workspaces to read their rules again.
    ///
    /// # Errors
    /// [`StoreError::SystemActor`], [`StoreError::RequestRuleInvalid`] over the per-scope limit
    /// or an empty host, [`StoreError::UnknownWorkspace`] for a workspace that is not theirs, or
    /// a database error.
    pub fn add_request_rule(&self, new: &NewRequestRule) -> Result<RequestRule, StoreError> {
        self.store.add_request_rule_by(self.user, new)
    }

    /// What this user's rules say about `host` for `workspace` right now, without recording
    /// anything: the answer [`Store::decide`] gives, but `None` where that would open a pending
    /// row. `workspace` may be a name no workspace has yet (a create not made yet): this user's
    /// global rules and sets then decide.
    #[must_use]
    pub fn peek(&self, workspace: &WorkspaceId, host: &Host) -> Option<Decision> {
        self.store.peek_by(self.user, workspace, host)
    }

    /// Allows each of `hosts` for `workspace` (theirs) that no rule decides yet, with an exact
    /// allow rule made by [`Actor::WorkspaceCreate`] (a create names the hosts of its
    /// repositories, and the clone that follows would otherwise wait in the inbox). A host that
    /// is allowed already, or that a deny decides, gets no rule: a create never overrides a
    /// block. Returns the rules made.
    ///
    /// # Errors
    /// [`StoreError::UnknownWorkspace`] for a workspace that is not theirs, or a database error;
    /// the rules made before it stay.
    fn allow_hosts_for_create(
        &self,
        workspace: &WorkspaceId,
        hosts: &[Host],
    ) -> Result<Vec<Rule>, StoreError> {
        let mut made = Vec::new();
        for host in hosts {
            if self.peek(workspace, host).is_some() {
                continue;
            }
            made.push(self.add_rule(&NewRule {
                scope: Scope::Workspace(workspace.clone()),
                pattern: Pattern::Exact(host.clone()),
                effect: Effect::Allow,
                expires_at: None,
                created_by: Actor::WorkspaceCreate,
            })?);
        }
        Ok(made)
    }
}
