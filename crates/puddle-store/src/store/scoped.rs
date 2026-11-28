// SPDX-License-Identifier: AGPL-3.0-or-later
//! The store as one user sees it.

use puddle_types::{Decision, Host, WorkspaceId};

use super::ssh_agent_asks::{SshAgentAnswer, SshAgentAsk};

use super::rows::load_pending;
use super::users::owner_in;
use super::{Decided, NewRequestRule, PendingId, PendingRow, RequestRule, Resolution, Store, lock};
use crate::error::StoreError;
use crate::rule::{Actor, NewRule, Precedence, Rule};
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
    /// with an exact allow rule made by [`Actor::WorkspaceCreate`](crate::rule::Actor::WorkspaceCreate) (a create names the hosts of
    /// its repositories, and the clone that follows would otherwise wait in the inbox). A host
    /// that is allowed already, or that a deny decides, gets no rule: a create never overrides a
    /// block. Returns the rules made.
    ///
    /// # Errors
    /// [`StoreError::UnknownWorkspace`] for a workspace with no recorded user, or a database
    /// error; nothing is made then.
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
        self.store.add_rule_by(self.user, new, None)
    }

    /// [`UserStore::add_rule`] with the precedence the rule takes (R-59). A rule made without
    /// one takes the default of the set it goes into, else 5.
    ///
    /// # Errors
    /// The same as [`UserStore::add_rule`].
    pub fn add_rule_with_precedence(
        &self,
        new: &NewRule,
        precedence: Precedence,
    ) -> Result<Rule, StoreError> {
        self.store.add_rule_by(self.user, new, Some(precedence))
    }

    /// Makes a rule on method and path of this user: global for their workspaces, or for one of
    /// them, and tells running workspaces to read their rules again.
    ///
    /// # Errors
    /// [`StoreError::SystemActor`], [`StoreError::RequestRuleInvalid`] over the per-scope limit,
    /// [`StoreError::UnknownWorkspace`] for a workspace that is not theirs, or
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

    /// Approves or denies an open pending row of this user's workspace (R-15 to R-17), in one
    /// transaction.
    ///
    /// # Errors
    /// [`StoreError::UnknownPending`] for a row of another user's workspace, and, for the rest,
    /// [`StoreError::PendingNotOpen`], [`StoreError::Pattern`], [`StoreError::ExpiryNotInFuture`],
    /// [`StoreError::ExactAllowOnly`], [`StoreError::SystemActor`], or a database error.
    pub fn resolve_pending(
        &self,
        id: PendingId,
        resolution: &Resolution,
        actor: Actor,
    ) -> Result<Decided, StoreError> {
        self.store
            .resolve_pending_by(self.user, id, resolution, actor)
    }

    /// One pending row of this user's workspaces.
    ///
    /// # Errors
    /// [`StoreError::UnknownPending`] for a row that is not theirs or does not exist, or a
    /// database error.
    pub fn pending(&self, id: PendingId) -> Result<PendingRow, StoreError> {
        let conn = lock(&self.store.conn);
        let row = load_pending(&conn, id)?;
        match owner_in(&conn, &row.workspace) {
            Ok(owner) if owner == self.user => Ok(row),
            Ok(_) | Err(StoreError::UnknownWorkspace(_)) => Err(StoreError::UnknownPending(id)),
            Err(err) => Err(err),
        }
    }

    /// Switches this user's rule on method and path on or off (R-65).
    ///
    /// # Errors
    /// [`StoreError::UnknownRequestRule`] for a rule that is not theirs, and, for the rest,
    /// [`StoreError::SystemActor`] or a database error.
    pub fn set_request_rule_enabled(
        &self,
        id: i64,
        enabled: bool,
        actor: Actor,
    ) -> Result<RequestRule, StoreError> {
        self.store
            .set_request_rule_enabled_by(self.user, id, enabled, actor)
    }

    /// Deletes this user's rule on method and path.
    ///
    /// # Errors
    /// [`StoreError::UnknownRequestRule`] for a rule that is not theirs, [`StoreError::SystemActor`]
    /// or a database error.
    pub fn delete_request_rule(&self, id: i64, actor: Actor) -> Result<RequestRule, StoreError> {
        self.store.delete_request_rule_by(self.user, id, actor)
    }

    /// The open questions of this user's workspaces, of one of them or all, most recent first.
    ///
    /// # Errors
    /// A database error, or [`StoreError::Corrupt`].
    pub fn open_ssh_agent_asks(
        &self,
        workspace: Option<&WorkspaceId>,
    ) -> Result<Vec<SshAgentAsk>, StoreError> {
        self.store
            .open_ssh_agent_asks_where(Some(self.user), workspace)
    }

    /// Answers the open question `id` of `workspace` (theirs).
    ///
    /// # Errors
    /// [`StoreError::UnknownSshAgentAsk`] for a question that is not this workspace's or whose
    /// workspace is not theirs, [`StoreError::SshAgentAskNotOpen`] for one already answered, or a
    /// database error.
    pub fn answer_ssh_agent_ask(
        &self,
        workspace: &WorkspaceId,
        id: i64,
        answer: SshAgentAnswer,
        actor: Actor,
    ) -> Result<SshAgentAsk, StoreError> {
        self.store
            .answer_ssh_agent_ask_by(self.user, workspace, id, answer, actor)
    }

    /// Allows each of `hosts` for `workspace` (theirs) that no rule decides yet, with an exact
    /// allow rule made by [`Actor::WorkspaceCreate`] (a create names the hosts of its
    /// repositories, and the clone that follows would otherwise wait in the inbox). A host that
    /// is allowed already, or that a deny decides, gets no rule: a create never overrides a
    /// block. Returns the rules made.
    ///
    /// # Errors
    /// [`StoreError::UnknownWorkspace`] for a workspace that is not theirs, or a database error;
    /// nothing is made then.
    fn allow_hosts_for_create(
        &self,
        workspace: &WorkspaceId,
        hosts: &[Host],
    ) -> Result<Vec<Rule>, StoreError> {
        self.store.allow_hosts_by(self.user, workspace, hosts)
    }
}
