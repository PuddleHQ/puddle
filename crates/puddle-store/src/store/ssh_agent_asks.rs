// SPDX-License-Identifier: AGPL-3.0-or-later
//! The filtered SSH agent's first-use question: the first signature for each workspace,
//! host and key waits for the user in the inbox.
//!
//! One open row stands for every request that waits on the same workspace, host and key. The user
//! answers it once (the waiting requests only), for this run of the workspace, or always; the last
//! two leave a grant that later requests find, so they ask nothing. A `run` grant goes when the
//! workspace stops; an `always` grant goes when the user turns the agent off for the host. Each
//! answer is written to the audit log in the transaction that makes it.

use puddle_types::{Event, WorkspaceId};
use rusqlite::{OptionalExtension, Transaction, params};

use super::audit::append_by;
use super::rows::corrupt;
use crate::audit::{AuditRecord, WorkspaceOpKind, WorkspaceOpOutcome, actor_str};
use crate::error::StoreError;
use crate::rule::Actor;
use crate::users::UserId;

use super::{Store, lock};

/// How long a decided row is kept for the requests still waiting on it to read.
const DECIDED_KEPT_MS: u64 = 3_600_000;

/// Most open questions one workspace may have: a guest that offers key after key stops here.
pub const MAX_OPEN_SSH_AGENT_ASKS: usize = 64;

/// What the user answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SshAgentAnswer {
    /// Sign for the requests waiting now.
    Once,
    /// Sign until the workspace stops.
    Run,
    /// Sign until the agent is turned off for the host.
    Always,
    /// Do not sign.
    Deny,
}

impl SshAgentAnswer {
    fn state(self) -> SshAgentAskState {
        match self {
            Self::Once => SshAgentAskState::AllowedOnce,
            Self::Run => SshAgentAskState::AllowedRun,
            Self::Always => SshAgentAskState::AllowedAlways,
            Self::Deny => SshAgentAskState::Denied,
        }
    }
}

/// Where a question is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SshAgentAskState {
    /// Waiting for the user.
    Requested,
    /// Allowed for the requests that waited.
    AllowedOnce,
    /// Allowed until the workspace stops.
    AllowedRun,
    /// Allowed until the agent is turned off for the host.
    AllowedAlways,
    /// Denied.
    Denied,
}

impl SshAgentAskState {
    /// `requested`, `allowed_once`, `allowed_run`, `allowed_always` or `denied`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Requested => "requested",
            Self::AllowedOnce => "allowed_once",
            Self::AllowedRun => "allowed_run",
            Self::AllowedAlways => "allowed_always",
            Self::Denied => "denied",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "requested" => Self::Requested,
            "allowed_once" => Self::AllowedOnce,
            "allowed_run" => Self::AllowedRun,
            "allowed_always" => Self::AllowedAlways,
            "denied" => Self::Denied,
            _ => return None,
        })
    }

    /// Whether the answer lets the request sign.
    #[must_use]
    pub fn allows(self) -> bool {
        matches!(
            self,
            Self::AllowedOnce | Self::AllowedRun | Self::AllowedAlways
        )
    }
}

/// One question to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshAgentAsk {
    /// Row id, never reused.
    pub id: i64,
    /// The workspace that asked.
    pub workspace: WorkspaceId,
    /// The host the login is for.
    pub host: String,
    /// The key's SHA-256 fingerprint (`SHA256:...`).
    pub fingerprint: String,
    /// The key's type (`ssh-ed25519`, `ssh-rsa`, ...).
    pub key_type: String,
    /// Where it is.
    pub state: SshAgentAskState,
    /// Epoch ms of the first request.
    pub first_seen: u64,
    /// Epoch ms of the latest request.
    pub last_seen: u64,
    /// How many requests the row stands for.
    pub attempts: u64,
    /// Epoch ms of the answer.
    pub decided_at: Option<u64>,
}

/// What [`Store::start_ssh_agent_ask`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SshAgentAskStart {
    /// A grant covers it: sign without asking.
    Granted,
    /// The user has to answer this row.
    Open(i64),
}

const COLUMNS: &str = "SELECT id, workspace_id, host, fingerprint, key_type, state, first_seen,
    last_seen, attempts, decided_at FROM ssh_agent_asks";

fn ms(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

fn read(r: &rusqlite::Row<'_>) -> Result<SshAgentAsk, StoreError> {
    let workspace: String = r.get(1)?;
    let state: String = r.get(5)?;
    let id: i64 = r.get(0)?;
    let bad = |what: &str| corrupt("ssh_agent_asks", id, what);
    Ok(SshAgentAsk {
        id,
        workspace: WorkspaceId::new(&workspace).map_err(|_| bad("workspace id"))?,
        host: r.get(2)?,
        fingerprint: r.get(3)?,
        key_type: r.get(4)?,
        state: SshAgentAskState::parse(&state).ok_or_else(|| bad("state"))?,
        first_seen: ms(r.get(6)?),
        last_seen: ms(r.get(7)?),
        attempts: ms(r.get(8)?),
        decided_at: r.get::<_, Option<i64>>(9)?.map(ms),
    })
}

fn changed(fx: &mut Vec<Event>, workspace: &WorkspaceId) {
    fx.push(Event::SshAgentAsksChanged {
        workspace: workspace.clone(),
    });
}

impl Store {
    /// A request to sign for `fingerprint` at `host` in `workspace`: a grant answers it at once,
    /// otherwise it joins the open question for that triple (opening one, and telling the UI,
    /// when there is none).
    ///
    /// # Errors
    /// [`StoreError::TooManySshAgentAsks`] when the workspace has too many questions open, or a
    /// database error.
    pub fn start_ssh_agent_ask(
        &self,
        workspace: &WorkspaceId,
        host: &str,
        fingerprint: &str,
        key_type: &str,
    ) -> Result<SshAgentAskStart, StoreError> {
        self.change(|tx, now, fx| {
            let granted = tx
                .query_row(
                    "SELECT 1 FROM ssh_agent_grants
                     WHERE workspace_id = ?1 AND host = ?2 AND fingerprint = ?3",
                    params![workspace.as_str(), host, fingerprint],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
            if granted {
                return Ok(SshAgentAskStart::Granted);
            }
            let now_i = i64::try_from(now).unwrap_or(i64::MAX);
            let open: Option<i64> = tx
                .query_row(
                    "SELECT id FROM ssh_agent_asks WHERE workspace_id = ?1 AND host = ?2
                     AND fingerprint = ?3 AND state = 'requested'",
                    params![workspace.as_str(), host, fingerprint],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(id) = open {
                tx.execute(
                    "UPDATE ssh_agent_asks SET attempts = attempts + 1, last_seen = ?2 WHERE id = ?1",
                    params![id, now_i],
                )?;
                return Ok(SshAgentAskStart::Open(id));
            }
            let count: i64 = tx.query_row(
                "SELECT count(*) FROM ssh_agent_asks WHERE workspace_id = ?1 AND state = 'requested'",
                [workspace.as_str()],
                |r| r.get(0),
            )?;
            if usize::try_from(count).unwrap_or(usize::MAX) >= MAX_OPEN_SSH_AGENT_ASKS {
                return Err(StoreError::TooManySshAgentAsks(MAX_OPEN_SSH_AGENT_ASKS));
            }
            tx.execute(
                "DELETE FROM ssh_agent_asks WHERE decided_at < ?1",
                [i64::try_from(now.saturating_sub(DECIDED_KEPT_MS)).unwrap_or(0)],
            )?;
            tx.execute(
                "INSERT INTO ssh_agent_asks (workspace_id, host, fingerprint, key_type, state,
                                             first_seen, last_seen, attempts)
                 VALUES (?1, ?2, ?3, ?4, 'requested', ?5, ?5, 1)",
                params![workspace.as_str(), host, fingerprint, key_type, now_i],
            )?;
            changed(fx, workspace);
            Ok(SshAgentAskStart::Open(tx.last_insert_rowid()))
        })
    }

    /// One question, or `None` once it was pruned.
    ///
    /// # Errors
    /// A database error, or [`StoreError::Corrupt`].
    pub fn ssh_agent_ask(&self, id: i64) -> Result<Option<SshAgentAsk>, StoreError> {
        let conn = lock(&self.conn);
        conn.query_row(&format!("{COLUMNS} WHERE id = ?1"), [id], |r| Ok(read(r)))
            .optional()?
            .transpose()
    }

    /// The open questions of `user`'s workspaces (every user's when `None`), of one of them or
    /// all, most recent first.
    ///
    /// # Errors
    /// A database error, or [`StoreError::Corrupt`].
    pub fn open_ssh_agent_asks(
        &self,
        user: Option<UserId>,
        workspace: Option<&WorkspaceId>,
    ) -> Result<Vec<SshAgentAsk>, StoreError> {
        let conn = lock(&self.conn);
        let mut stmt = conn.prepare(&format!(
            "{COLUMNS} WHERE state = 'requested' AND (?1 IS NULL OR workspace_id = ?1)
               AND (?2 IS NULL OR workspace_id IN
                    (SELECT workspace_id FROM workspace_owners WHERE user_id = ?2))
             ORDER BY last_seen DESC, id DESC"
        ))?;
        let args = params![workspace.map(WorkspaceId::as_str), user.map(UserId::get)];
        let rows = stmt.query_map(args, |r| Ok(read(r)))?;
        rows.map(|row| row?).collect()
    }

    /// Answers the open question `id` of `workspace`. `Run` and `Always` leave a grant.
    ///
    /// # Errors
    /// [`StoreError::UnknownSshAgentAsk`] for a question that is not this workspace's,
    /// [`StoreError::SshAgentAskNotOpen`] for one already answered, or a database error.
    pub fn answer_ssh_agent_ask(
        &self,
        workspace: &WorkspaceId,
        id: i64,
        answer: SshAgentAnswer,
        actor: Actor,
    ) -> Result<SshAgentAsk, StoreError> {
        self.change(|tx, now, fx| {
            let ask = tx
                .query_row(&format!("{COLUMNS} WHERE id = ?1"), [id], |r| Ok(read(r)))
                .optional()?
                .transpose()?
                .filter(|ask| ask.workspace == *workspace)
                .ok_or(StoreError::UnknownSshAgentAsk(id))?;
            if ask.state != SshAgentAskState::Requested {
                return Err(StoreError::SshAgentAskNotOpen {
                    id,
                    state: ask.state.as_str(),
                });
            }
            let state = answer.state();
            let now_i = i64::try_from(now).unwrap_or(i64::MAX);
            tx.execute(
                "UPDATE ssh_agent_asks SET state = ?2, decided_at = ?3 WHERE id = ?1",
                params![id, state.as_str(), now_i],
            )?;
            if matches!(answer, SshAgentAnswer::Run | SshAgentAnswer::Always) {
                tx.execute(
                    "INSERT INTO ssh_agent_grants (workspace_id, host, fingerprint, run, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5)
                     ON CONFLICT (workspace_id, host, fingerprint)
                     DO UPDATE SET run = ?4, created_at = ?5",
                    params![
                        workspace.as_str(),
                        ask.host,
                        ask.fingerprint,
                        i64::from(answer == SshAgentAnswer::Run),
                        now_i
                    ],
                )?;
            }
            append_by(
                tx,
                &AuditRecord::WorkspaceOp {
                    ts: now,
                    op: WorkspaceOpKind::SshAgentAnswered,
                    workspace_id: Some(workspace.as_str().to_owned()),
                    outcome: WorkspaceOpOutcome::Succeeded,
                    actor: actor_str(actor),
                    detail: Some(format!(
                        "{} for {} key {} to {}",
                        state.as_str().replace('_', " "),
                        ask.key_type,
                        ask.fingerprint,
                        ask.host
                    )),
                },
                None,
            )?;
            changed(fx, workspace);
            Ok(SshAgentAsk {
                state,
                decided_at: Some(now),
                ..ask
            })
        })
    }

    /// Ends a run of `workspace`, or of every workspace: its `run` grants go, and so do its open
    /// questions, which nobody waits on any more (the guest's `ssh` died with the workspace).
    /// Called as a workspace stops, and as Puddle starts, when none can be running.
    ///
    /// # Errors
    /// A database error; nothing changes then.
    pub fn end_ssh_agent_run(&self, workspace: Option<&WorkspaceId>) -> Result<(), StoreError> {
        self.change(|tx, _now, fx| {
            let which = workspace.map(WorkspaceId::as_str);
            tx.execute(
                "DELETE FROM ssh_agent_grants WHERE run = 1 AND (?1 IS NULL OR workspace_id = ?1)",
                [which],
            )?;
            let mut stmt = tx.prepare(
                "SELECT DISTINCT workspace_id FROM ssh_agent_asks
                 WHERE state = 'requested' AND (?1 IS NULL OR workspace_id = ?1)",
            )?;
            let waiting = stmt
                .query_map([which], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            drop(stmt);
            tx.execute(
                "DELETE FROM ssh_agent_asks
                 WHERE state = 'requested' AND (?1 IS NULL OR workspace_id = ?1)",
                [which],
            )?;
            for id in waiting {
                if let Ok(id) = WorkspaceId::new(&id) {
                    changed(fx, &id);
                }
            }
            Ok(())
        })
    }
}

/// Drops what the agent held for `host` once the user turns it off there: its grants and its
/// open questions.
pub(super) fn drop_host(
    tx: &Transaction<'_>,
    workspace: &WorkspaceId,
    host: &str,
) -> Result<bool, StoreError> {
    tx.execute(
        "DELETE FROM ssh_agent_grants WHERE workspace_id = ?1 AND host = ?2",
        params![workspace.as_str(), host],
    )?;
    let closed = tx.execute(
        "DELETE FROM ssh_agent_asks WHERE workspace_id = ?1 AND host = ?2 AND state = 'requested'",
        params![workspace.as_str(), host],
    )?;
    Ok(closed > 0)
}

/// Removes what a deleted workspace held.
pub(super) fn delete_workspace_rows(
    tx: &Transaction<'_>,
    workspace: &WorkspaceId,
) -> Result<(), StoreError> {
    for table in ["ssh_agent_asks", "ssh_agent_grants"] {
        tx.execute(
            &format!("DELETE FROM {table} WHERE workspace_id = ?1"),
            [workspace.as_str()],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
