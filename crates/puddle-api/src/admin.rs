// SPDX-License-Identifier: AGPL-3.0-or-later
//! The admin screen's server side: the user list and the server's totals (counts and sizes, never
//! what a user keeps), making a user with an invite, reissuing an invite, and deleting a user.
//!
//! Secure by design: nothing here reads a password hash, a device key, a session or a credential
//! (the accounts' secrets are reached only through [`SignIn`], which never returns one except the
//! invite it has just made, to the owner who asked), and the owner has no way into another user's
//! workspace data: workspaces are counted and deleted, never opened, listed by name or read.
//!
//! Deleting a user is a state, not one call: the account is locked out and marked deleting at
//! once, and the [`reaper`](AdminService::run_reaper) stops and deletes their workspaces, then
//! removes the rest. It runs on a timer and when asked, so a deletion the command line began (while
//! the server was down) finishes at the next start, and one that fails half way is tried again.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use puddle_store::{AdminAction, Role, Store, UserFacts, UserId};
use puddle_types::{WorkspaceId, WorkspaceStatus};
use tokio::sync::{Notify, watch};

use crate::error::{ApiError, ErrorCode, blocking};
use crate::signin::SignIn;
use crate::wire::{
    AdminActionKind, AdminAuditLine, AdminAuditList, AdminStats, AdminUser, AdminUserList,
    AdminUserState, InviteIssued,
};
use crate::workspaces::{WorkspaceRecord, WorkspaceService};

/// How often the reaper looks for deletions to finish when nobody asks.
const REAP_EVERY: Duration = Duration::from_secs(10);

/// The most audit lines the screen asks for.
pub(crate) const AUDIT_LINES: u32 = 100;

/// The admin screen's server side.
pub(crate) struct AdminService {
    store: Arc<Store>,
    sign_in: Arc<SignIn>,
    workspaces: Arc<dyn WorkspaceService>,
    reap: Notify,
}

fn conflict(message: &str) -> ApiError {
    ApiError::new(StatusCode::CONFLICT, ErrorCode::Conflict, message)
}

/// Whether the workspace has a sandbox that is up or coming up.
fn is_up(status: WorkspaceStatus) -> bool {
    matches!(
        status,
        WorkspaceStatus::Starting
            | WorkspaceStatus::Running
            | WorkspaceStatus::Draining
            | WorkspaceStatus::Paused
    )
}

fn mib_used(record: &WorkspaceRecord) -> u64 {
    record
        .disk_used_mib
        .unwrap_or(0)
        .saturating_add(record.root_disk_used_mib.unwrap_or(0))
}

fn action_kind(action: AdminAction) -> AdminActionKind {
    match action {
        AdminAction::UserCreated => AdminActionKind::UserCreated,
        AdminAction::InviteReissued => AdminActionKind::InviteReissued,
        AdminAction::UserDeleteStarted => AdminActionKind::UserDeleteStarted,
        AdminAction::UserDeleted => AdminActionKind::UserDeleted,
    }
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

impl AdminService {
    pub(crate) fn new(
        store: Arc<Store>,
        sign_in: Arc<SignIn>,
        workspaces: Arc<dyn WorkspaceService>,
    ) -> Self {
        Self {
            store,
            sign_in,
            workspaces,
            reap: Notify::new(),
        }
    }

    async fn all_workspaces(&self) -> Result<Vec<WorkspaceRecord>, ApiError> {
        Ok(self.workspaces.list().await?)
    }

    fn view(facts: &UserFacts, has_password: bool, mine: &[&WorkspaceRecord]) -> AdminUser {
        let state = if facts.deleting_since.is_some() {
            AdminUserState::Deleting
        } else if has_password {
            AdminUserState::Active
        } else {
            AdminUserState::Invited
        };
        let running = mine.iter().filter(|w| is_up(w.status)).count();
        AdminUser {
            id: facts.user.id.get(),
            name: facts.user.name.clone(),
            role: facts.user.role.as_str().to_owned(),
            state,
            created_at: facts.user.created_at,
            last_sign_in: facts.last_sign_in,
            devices: facts.devices,
            workspaces_running: count(running),
            workspaces_stopped: count(mine.len() - running),
            disk_used_mib: mine.iter().map(|w| mib_used(w)).sum(),
        }
    }

    /// The accounts with their counts.
    pub(crate) async fn users(&self) -> Result<AdminUserList, ApiError> {
        let store = self.store.clone();
        let (facts, owned) = blocking(move || {
            let facts = store.user_facts()?;
            let owned = facts
                .iter()
                .map(|f| Ok((f.user.id, store.workspaces_of(f.user.id)?)))
                .collect::<Result<Vec<_>, ApiError>>()?;
            Ok((facts, owned))
        })
        .await?;
        let passwords = self
            .sign_in
            .have_passwords(facts.iter().map(|f| f.user.id).collect())
            .await?;
        let records = self.all_workspaces().await?;
        let users = facts
            .iter()
            .zip(passwords)
            .zip(owned)
            .map(|((facts, has_password), (_, ids))| {
                let ids: HashSet<&WorkspaceId> = ids.iter().collect();
                let mine: Vec<&WorkspaceRecord> =
                    records.iter().filter(|w| ids.contains(&w.id)).collect();
                Self::view(facts, has_password, &mine)
            })
            .collect();
        Ok(AdminUserList { users })
    }

    /// The server's totals.
    pub(crate) async fn stats(&self) -> Result<AdminStats, ApiError> {
        let store = self.store.clone();
        let users = blocking(move || Ok(store.users()?.len())).await?;
        let records = self.all_workspaces().await?;
        let up = records.iter().filter(|w| is_up(w.status));
        Ok(AdminStats {
            users: count(users),
            workspaces: count(records.len()),
            running: count(up.clone().count()),
            disk_used_mib: records.iter().map(mib_used).sum(),
            memory_mib: up.map(|w| u64::from(w.memory.get())).sum(),
            host_cpus: std::thread::available_parallelism()
                .map_or(1, |n| u32::try_from(n.get()).unwrap_or(u32::MAX)),
        })
    }

    fn issued(user: &puddle_store::User, invite: &crate::ResetInvite) -> InviteIssued {
        InviteIssued {
            user: AdminUser {
                id: user.id.get(),
                name: user.name.clone(),
                role: user.role.as_str().to_owned(),
                state: AdminUserState::Invited,
                created_at: user.created_at,
                last_sign_in: None,
                devices: 0,
                workspaces_running: 0,
                workspaces_stopped: 0,
                disk_used_mib: 0,
            },
            invite_secret: invite.secret.as_str().to_owned(),
            expires_at: invite.expires_at,
        }
    }

    /// Makes the account `name` in the invited state and its invite.
    pub(crate) async fn create(&self, actor: UserId, name: &str) -> Result<InviteIssued, ApiError> {
        let (user, invite) = self.sign_in.invite_new_user(actor, name).await?;
        Ok(Self::issued(&user, &invite))
    }

    /// A new invite for `target`, who must exist and not be going away. The owner's own password
    /// is reset from the server's command line, not here.
    pub(crate) async fn reissue(
        &self,
        actor: UserId,
        target: UserId,
    ) -> Result<InviteIssued, ApiError> {
        let store = self.store.clone();
        let user = blocking(move || Ok(store.user_facts()?)).await?;
        let facts = user
            .into_iter()
            .find(|f| f.user.id == target)
            .ok_or_else(|| ApiError::not_found(format!("no user {}", target.get())))?;
        if facts.deleting_since.is_some() {
            return Err(conflict("this user is being deleted"));
        }
        if facts.user.role == Role::Owner {
            return Err(conflict(
                "the owner's password is reset with the `user reset` command on the server",
            ));
        }
        let invite = self.sign_in.reissue_invite(actor, target).await?;
        Ok(Self::issued(&facts.user, &invite))
    }

    /// Starts deleting `target`: `confirm` must be their name exactly. They are locked out at
    /// once; their workspaces and the rest follow in the background.
    pub(crate) async fn delete(
        &self,
        actor: UserId,
        target: UserId,
        confirm: &str,
    ) -> Result<(), ApiError> {
        if target == actor {
            return Err(conflict("you can't delete your own account"));
        }
        let store = self.store.clone();
        let user = blocking(move || Ok(store.user(target)?))
            .await
            .map_err(|_| ApiError::not_found(format!("no user {}", target.get())))?;
        if user.role == Role::Owner {
            return Err(conflict("the owner can't be deleted"));
        }
        if confirm != user.name {
            return Err(ApiError::invalid(
                "type the user's name exactly to confirm the deletion",
            ));
        }
        let store = self.store.clone();
        blocking(move || Ok(store.begin_user_delete(actor, target)?)).await?;
        self.sign_in.lock_out(target).await?;
        self.reap.notify_one();
        Ok(())
    }

    /// The newest admin audit lines.
    pub(crate) async fn audit(&self) -> Result<AdminAuditList, ApiError> {
        let store = self.store.clone();
        let lines = blocking(move || Ok(store.admin_audit(AUDIT_LINES)?)).await?;
        Ok(AdminAuditList {
            lines: lines
                .into_iter()
                .map(|l| AdminAuditLine {
                    at: l.at,
                    actor: l.actor_name,
                    action: action_kind(l.action),
                    target_id: l.target.get(),
                    target: l.target_name,
                })
                .collect(),
        })
    }

    /// One look at every deletion under way: stops what runs, deletes what is stopped, and
    /// removes the user once none of their workspaces is left. A step that fails is logged and
    /// tried again next time.
    pub(crate) async fn reap_once(&self) {
        let store = self.store.clone();
        let deleting = match blocking(move || Ok(store.users_deleting()?)).await {
            Ok(users) => users,
            Err(err) => {
                tracing::warn!(error = ?err, "could not list the users being deleted");
                return;
            }
        };
        for user in deleting {
            if let Err(err) = self.reap_user(user.id).await {
                tracing::warn!(user = user.id.get(), error = ?err, "deleting a user is not done yet");
            }
        }
    }

    async fn reap_user(&self, user: UserId) -> Result<(), ApiError> {
        let store = self.store.clone();
        let ids: HashSet<WorkspaceId> = blocking(move || Ok(store.workspaces_of(user)?))
            .await?
            .into_iter()
            .collect();
        let left: Vec<WorkspaceRecord> = self
            .all_workspaces()
            .await?
            .into_iter()
            .filter(|w| ids.contains(&w.id))
            .collect();
        if left.is_empty() {
            let store = self.store.clone();
            return blocking(move || Ok(store.finish_user_delete(user)?)).await;
        }
        for record in left {
            if record.busy.is_some() {
                continue;
            }
            let step = if is_up(record.status) {
                self.workspaces.stop(&record.id).await.map(|_| ())
            } else {
                // The owner confirmed in the dialog that this loses everything of the user's,
                // so the delete goes ahead with the fingerprint of what it found.
                let check = self.workspaces.delete_check(&record.id).await?;
                self.workspaces
                    .delete(&record.id, Some(&check.fingerprint))
                    .await
                    .map(|_| ())
            };
            if let Err(err) = step {
                tracing::warn!(workspace = %record.id, error = %err, "a deleted user's workspace is not gone yet");
            }
        }
        Ok(())
    }

    /// Runs [`AdminService::reap_once`] on a timer and whenever a deletion starts, until the
    /// server shuts down.
    pub(crate) async fn run_reaper(self: Arc<Self>, mut shutdown: watch::Receiver<bool>) {
        loop {
            self.reap_once().await;
            tokio::select! {
                () = self.reap.notified() => {}
                () = tokio::time::sleep(REAP_EVERY) => {}
                _ = shutdown.changed() => return,
            }
        }
    }
}

#[cfg(test)]
mod tests;
