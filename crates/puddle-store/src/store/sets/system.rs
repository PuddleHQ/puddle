// SPDX-License-Identifier: AGPL-3.0-or-later
//! System managed: the hosts Puddle allows because of the user's own choices, and why.

use std::collections::{BTreeMap, BTreeSet};

use puddle_types::{Event, PendingId, WorkspaceId};
use rusqlite::params;

use super::index::{load_hidden, load_index, load_reasons};
use super::switches::close_rows_now_decided;
use crate::audit::AuditRecord;
use crate::catalogue::{self, SystemReason};
use crate::error::StoreError;
use crate::rule::Actor;
use crate::store::{AllUsers, Store, append, lock, users};
use crate::users::UserId;

/// One host Puddle allows because of the user's setup (R-40, R-41).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemHost {
    /// The pattern as written in the catalogue (`*.gallery.vsassets.io`).
    pub pattern: &'static str,
    /// What the host is for.
    pub note: &'static str,
    /// Why it is allowed.
    pub reason: SystemReason,
    /// The one workspace it is allowed for, or `None` for every workspace.
    pub workspace: Option<WorkspaceId>,
}

/// Which System managed reasons apply where, as derived from the user's setup (R-41).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SystemPlan {
    /// Reasons for every workspace (global choices).
    pub everywhere: BTreeSet<SystemReason>,
    /// Reasons for one workspace each (per-workspace choices).
    pub workspaces: BTreeMap<WorkspaceId, BTreeSet<SystemReason>>,
    /// Slugs of built-in sets to hide, because the feature they serve is off. A hidden set
    /// decides nothing, is left out of [`Store::rule_sets`] and can't be switched; its stored
    /// switches are kept, so it comes back as the user left it.
    pub hidden_sets: BTreeSet<&'static str>,
}

/// Each host of each reason, in catalogue order per reason.
pub(super) fn system_hosts(reasons: Vec<(Option<WorkspaceId>, SystemReason)>) -> Vec<SystemHost> {
    reasons
        .into_iter()
        .flat_map(|(workspace, reason)| {
            reason.hosts().iter().map(move |entry| SystemHost {
                pattern: entry.pattern,
                note: entry.note,
                reason,
                workspace: workspace.clone(),
            })
        })
        .collect()
}

impl Store {
    /// The System managed hosts of every workspace and why each is allowed (R-40), in catalogue
    /// order per reason. For the install's own jobs; what a user is shown comes from
    /// [`Store::system_managed`].
    ///
    /// # Errors
    /// A database error.
    pub fn system_managed_every_user(&self, _: &AllUsers) -> Result<Vec<SystemHost>, StoreError> {
        let reasons = load_reasons(&lock(&self.conn))?;
        Ok(system_hosts(reasons))
    }

    /// The System managed hosts `user`'s requests meet (R-40): the ones for every workspace (the
    /// install's own choices) and the ones for `user`'s workspaces, in catalogue order per reason.
    ///
    /// # Errors
    /// A database error.
    pub fn system_managed(&self, user: UserId) -> Result<Vec<SystemHost>, StoreError> {
        let conn = lock(&self.conn);
        let owners = users::load_owners(&conn)?;
        let mut reasons = load_reasons(&conn)?;
        reasons.retain(|(workspace, _)| {
            workspace
                .as_ref()
                .is_none_or(|w| owners.get(w) == Some(&user))
        });
        Ok(system_hosts(reasons))
    }

    /// Replaces the System managed reasons with `plan`, derived from the user's setup (R-41).
    /// Records each scope whose reasons changed and closes the open requests the new hosts
    /// decide (by `system`). Returns the rows it closed; a plan equal to the stored one changes
    /// and records nothing.
    ///
    /// # Errors
    /// A database error; nothing changes then.
    pub fn set_system_managed(&self, plan: &SystemPlan) -> Result<Vec<PendingId>, StoreError> {
        let mut wanted: BTreeMap<Option<WorkspaceId>, BTreeSet<SystemReason>> = BTreeMap::new();
        if !plan.everywhere.is_empty() {
            wanted.insert(None, plan.everywhere.clone());
        }
        for (workspace, reasons) in &plan.workspaces {
            if !reasons.is_empty() {
                wanted.insert(Some(workspace.clone()), reasons.clone());
            }
        }
        let hidden_wanted: BTreeSet<String> = plan
            .hidden_sets
            .iter()
            .filter(|slug| catalogue::built_in(slug).is_some())
            .map(ToString::to_string)
            .collect();
        let closed = self.change(|tx, now, fx| {
            // Read here, under the write lock: another writer's plan must not be recorded twice.
            let mut current: BTreeMap<Option<WorkspaceId>, BTreeSet<SystemReason>> =
                BTreeMap::new();
            for (workspace, reason) in load_reasons(tx)? {
                current.entry(workspace).or_default().insert(reason);
            }
            if current == wanted && load_hidden(tx)? == hidden_wanted {
                return Ok(Vec::new());
            }
            tx.execute("DELETE FROM hidden_builtin_sets", [])?;
            for slug in &hidden_wanted {
                tx.execute(
                    "INSERT INTO hidden_builtin_sets (slug) VALUES (?1)",
                    params![slug],
                )?;
            }
            tx.execute("DELETE FROM system_reasons", [])?;
            for (workspace, reasons) in &wanted {
                for reason in reasons {
                    tx.execute(
                        "INSERT INTO system_reasons (workspace_id, reason) VALUES (?1, ?2)",
                        params![workspace.as_ref().map(WorkspaceId::as_str), reason.as_str()],
                    )?;
                }
            }
            let scopes: BTreeSet<&Option<WorkspaceId>> =
                current.keys().chain(wanted.keys()).collect();
            let empty = BTreeSet::new();
            for scope in scopes {
                let before = current.get(scope).unwrap_or(&empty);
                let after = wanted.get(scope).unwrap_or(&empty);
                if before == after {
                    continue;
                }
                let record = AuditRecord::SystemManagedChanged {
                    ts: now,
                    workspace_id: scope.as_ref().map(ToString::to_string),
                    added: after
                        .difference(before)
                        .map(|r| r.as_str().to_owned())
                        .collect(),
                    removed: before
                        .difference(after)
                        .map(|r| r.as_str().to_owned())
                        .collect(),
                };
                append(tx, &record)?;
            }
            let index = load_index(tx)?;
            let closed = close_rows_now_decided(tx, &index, None, None, Actor::System, now, fx)?;
            fx.push(Event::RulesChanged {});
            Ok(closed)
        })?;
        tracing::info!(closed = closed.len(), "System managed hosts changed");
        Ok(closed)
    }
}
