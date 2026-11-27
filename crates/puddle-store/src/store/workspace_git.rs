// SPDX-License-Identifier: AGPL-3.0-or-later
//! Each workspace's Git settings and its repository list in the store.

use puddle_types::{Event, WorkspaceId};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use super::identities::{all, attached, changed_for, check_list, write_list};
use super::{Store, lock, sql_ts, users};
use crate::error::StoreError;
use crate::identity::{CredentialChoice, Identity, IdentityId, resolve};
use crate::users::UserId;
use crate::workspace_git::{
    GitDefaults, GitStart, PushBlock, RepoEntry, RepoGitRequest, RepoRef, RepoToggles, StartBasis,
    SwitchChange, SwitchSource, WorkspaceGit,
};

impl Store {
    /// A workspace's Git settings: its identities in order, its repository table and the two
    /// switches. A workspace nobody configured gets its user's defaults.
    ///
    /// # Errors
    /// A database error, or a stored row that doesn't parse.
    pub fn workspace_git(&self, ws: &WorkspaceId) -> Result<WorkspaceGit, StoreError> {
        let conn = lock(&self.conn);
        let (own_push, own_pull) = conn
            .query_row(
                "SELECT only_push_listed, only_pull_listed FROM workspace_git WHERE workspace_id = ?1",
                [ws.as_str()],
                |r| Ok((r.get::<_, Option<bool>>(0)?, r.get::<_, Option<bool>>(1)?)),
            )
            .optional()?
            .unwrap_or((None, None));
        let defaults = defaults_of(&conn, users::owner_in(&conn, ws)?)?;
        let only_push_listed = own_push.unwrap_or(defaults.only_push_listed);
        let only_pull_listed = own_pull.unwrap_or(defaults.only_pull_listed);
        let mut stmt = conn.prepare(
            "SELECT id, host, owner, repo, pull, push, push_block, created_at FROM workspace_repos
             WHERE workspace_id = ?1 ORDER BY id",
        )?;
        let rows = stmt
            .query_map([ws.as_str()], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, bool>(4)?,
                    r.get::<_, bool>(5)?,
                    r.get::<_, Option<String>>(6)?,
                    r.get::<_, i64>(7)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let repos = rows
            .into_iter()
            .map(|(id, host, owner, repo, pull, push, block, at)| {
                let repo = RepoRef::new(&host, &owner, &repo).map_err(|e| StoreError::Corrupt {
                    table: "workspace_repos",
                    id,
                    reason: e.to_string(),
                })?;
                Ok(RepoEntry {
                    id,
                    repo,
                    pull,
                    push,
                    push_block: block.as_deref().and_then(PushBlock::from_stored),
                    created_at: u64::try_from(at).unwrap_or(0),
                })
            })
            .collect::<Result<Vec<_>, StoreError>>()?;
        Ok(WorkspaceGit {
            identities: attached(&conn, ws)?,
            repos,
            only_push_listed,
            only_pull_listed,
            push_from: SwitchSource::of(own_push),
            pull_from: SwitchSource::of(own_pull),
        })
    }

    /// Checks that setting up `requests` in order (as [`Store::start_repository_git`] does) on
    /// the workspace `ws`, or on a new workspace of `user` (`None`, whose list is empty), would
    /// work: every named identity is the workspace's user's and no two identities on the
    /// workspace's list would cover the same place. Changes nothing.
    ///
    /// # Errors
    /// [`StoreError::UnknownIdentity`], [`StoreError::IdentityCollision`] naming both, or a
    /// database error.
    pub fn check_repository_git(
        &self,
        user: UserId,
        ws: Option<&WorkspaceId>,
        requests: &[RepoGitRequest],
    ) -> Result<(), StoreError> {
        let conn = lock(&self.conn);
        let (everyone, mut list) = match ws {
            Some(ws) => (
                all(&conn, users::owner_in(&conn, ws)?)?,
                attached(&conn, ws)?,
            ),
            None => (all(&conn, user)?, Vec::new()),
        };
        for request in requests {
            let (chosen, _) = choose(&everyone, &list, request)?;
            if let Some(identity) = chosen
                && !list.iter().any(|i| i.id == identity.id)
            {
                list.push(identity.clone());
                check_list(&list)?;
            }
        }
        Ok(())
    }

    /// Sets up one repository of the workspace `ws` before it is cloned: the
    /// identity the request names, else the one on the workspace's list that covers the
    /// repository, else the one that covers it of all your identities (the first in your order
    /// when several do), else the default identity, joins the list unless it is on it already;
    /// the repository gets its table row with the request's Pull and Push, and a row that exists
    /// keeps its toggles. Running it again changes nothing, so a clone that is tried again can
    /// call it again. The answer says which identity serves the repository and why.
    ///
    /// # Errors
    /// [`StoreError::UnknownIdentity`], [`StoreError::IdentityCollision`] when the identity would
    /// cover what another on the list covers, or a database error. Nothing changes on an error.
    pub fn start_repository_git(
        &self,
        ws: &WorkspaceId,
        request: &RepoGitRequest,
    ) -> Result<GitStart, StoreError> {
        self.change(|tx, now, fx| start_in(tx, now, fx, ws, request))
    }

    /// Sets up every repository of a new workspace's list, in order, in one transaction (as
    /// [`Store::start_repository_git`] does for one): identities join the list as they are
    /// chosen, so a later repository sees the earlier ones' identities. The answers are in the
    /// order of `requests`.
    ///
    /// # Errors
    /// As [`Store::start_repository_git`]; nothing of the list is set up on an error.
    pub fn start_repositories_git(
        &self,
        ws: &WorkspaceId,
        requests: &[RepoGitRequest],
    ) -> Result<Vec<GitStart>, StoreError> {
        self.change(|tx, now, fx| {
            requests
                .iter()
                .map(|request| start_in(tx, now, fx, ws, request))
                .collect()
        })
    }

    /// The default of the two switches for the workspaces of `user` that have not set their own.
    ///
    /// # Errors
    /// A database error.
    pub fn git_defaults(&self, user: UserId) -> Result<GitDefaults, StoreError> {
        defaults_of(&lock(&self.conn), user)
    }

    /// Sets the default of either switch for `user`; `None` leaves it as it is. Every workspace
    /// of theirs that has not set its own follows at once (an `identities_changed` event tells
    /// the screens).
    ///
    /// # Errors
    /// A database error.
    pub fn set_git_defaults(
        &self,
        user: UserId,
        only_push_listed: Option<bool>,
        only_pull_listed: Option<bool>,
    ) -> Result<GitDefaults, StoreError> {
        // Read and written in one transaction, so two changes of different switches both stick.
        self.change(|tx, _now, fx| {
            let now = defaults_of(tx, user)?;
            let next = GitDefaults {
                only_push_listed: only_push_listed.unwrap_or(now.only_push_listed),
                only_pull_listed: only_pull_listed.unwrap_or(now.only_pull_listed),
            };
            if next != now {
                tx.execute(
                    "INSERT INTO git_defaults (user_id, only_push_listed, only_pull_listed) VALUES (?3, ?1, ?2)
                     ON CONFLICT (user_id) DO UPDATE SET only_push_listed = ?1, only_pull_listed = ?2",
                    params![next.only_push_listed, next.only_pull_listed, user.get()],
                )?;
                changed_for(fx, Vec::new());
            }
            Ok(next)
        })
    }

    /// Sets either "only listed" switch for this workspace; `None` leaves it as it is (a switch
    /// the workspace never set follows the default).
    ///
    /// # Errors
    /// A database error.
    pub fn set_git_switches(
        &self,
        ws: &WorkspaceId,
        only_push_listed: Option<bool>,
        only_pull_listed: Option<bool>,
    ) -> Result<WorkspaceGit, StoreError> {
        self.change_git_switches(
            ws,
            only_push_listed.map_or(SwitchChange::Keep, SwitchChange::Set),
            only_pull_listed.map_or(SwitchChange::Keep, SwitchChange::Set),
        )
    }

    /// Changes either "only listed" switch for this workspace in one transaction: keeps it, sets
    /// the workspace's own value, or drops its own value so it follows the default again.
    ///
    /// # Errors
    /// A database error.
    pub fn change_git_switches(
        &self,
        ws: &WorkspaceId,
        push: SwitchChange,
        pull: SwitchChange,
    ) -> Result<WorkspaceGit, StoreError> {
        self.change(|tx, _now, fx| {
            let (own_push, own_pull) = tx
                .query_row(
                    "SELECT only_push_listed, only_pull_listed FROM workspace_git WHERE workspace_id = ?1",
                    [ws.as_str()],
                    |r| Ok((r.get::<_, Option<bool>>(0)?, r.get::<_, Option<bool>>(1)?)),
                )
                .optional()?
                .unwrap_or((None, None));
            tx.execute(
                "INSERT INTO workspace_git (workspace_id, only_push_listed, only_pull_listed)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT (workspace_id) DO UPDATE
                 SET only_push_listed = ?2, only_pull_listed = ?3",
                params![ws.as_str(), push.apply(own_push), pull.apply(own_pull)],
            )?;
            fx.push(Event::WorkspaceGitChanged {
                workspace: ws.clone(),
            });
            Ok(())
        })?;
        self.workspace_git(ws)
    }

    /// Adds a repository to the table.
    ///
    /// # Errors
    /// [`StoreError::RepoListed`] when it is already there, or a database error.
    pub fn add_repo(
        &self,
        ws: &WorkspaceId,
        repo: &RepoRef,
        toggles: RepoToggles,
        push_block: Option<PushBlock>,
    ) -> Result<RepoEntry, StoreError> {
        let id = self.change(|tx, now, fx| {
            let added = tx.execute(
                "INSERT INTO workspace_repos (workspace_id, host, owner, repo, pull, push, push_block, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT DO NOTHING",
                params![
                    ws.as_str(),
                    repo.host.as_str(),
                    repo.owner.as_str(),
                    repo.repo,
                    toggles.pull,
                    toggles.push,
                    push_block.map(PushBlock::as_str),
                    sql_ts(now)
                ],
            )?;
            if added == 0 {
                return Err(StoreError::RepoListed(repo.to_string()));
            }
            fx.push(Event::WorkspaceGitChanged { workspace: ws.clone() });
            Ok(tx.last_insert_rowid())
        })?;
        self.repo_entry(ws, id)
    }

    fn repo_entry(&self, ws: &WorkspaceId, id: i64) -> Result<RepoEntry, StoreError> {
        self.workspace_git(ws)?
            .repos
            .into_iter()
            .find(|entry| entry.id == id)
            .ok_or(StoreError::UnknownRepo(id))
    }

    /// Sets a row's Pull and Push toggles.
    ///
    /// # Errors
    /// [`StoreError::UnknownRepo`], or a database error.
    pub fn set_repo_toggles(
        &self,
        ws: &WorkspaceId,
        id: i64,
        toggles: RepoToggles,
    ) -> Result<RepoEntry, StoreError> {
        self.change(|tx, _now, fx| {
            let n = tx.execute(
                "UPDATE workspace_repos SET pull = ?3, push = ?4 WHERE id = ?1 AND workspace_id = ?2",
                params![id, ws.as_str(), toggles.pull, toggles.push],
            )?;
            if n == 0 {
                return Err(StoreError::UnknownRepo(id));
            }
            fx.push(Event::WorkspaceGitChanged { workspace: ws.clone() });
            Ok(())
        })?;
        self.repo_entry(ws, id)
    }

    /// Removes a row from the table.
    ///
    /// # Errors
    /// [`StoreError::UnknownRepo`], or a database error.
    pub fn remove_repo(&self, ws: &WorkspaceId, id: i64) -> Result<(), StoreError> {
        self.change(|tx, _now, fx| {
            let n = tx.execute(
                "DELETE FROM workspace_repos WHERE id = ?1 AND workspace_id = ?2",
                params![id, ws.as_str()],
            )?;
            if n == 0 {
                return Err(StoreError::UnknownRepo(id));
            }
            fx.push(Event::WorkspaceGitChanged {
                workspace: ws.clone(),
            });
            Ok(())
        })
    }
}

/// The identity `request` gets on a workspace whose list is `list`, from `everyone` (in your
/// order), and why (see [`Store::start_repository_git`]).
fn choose<'a>(
    everyone: &'a [Identity],
    list: &'a [Identity],
    request: &RepoGitRequest,
) -> Result<(Option<&'a Identity>, StartBasis), StoreError> {
    let (host, owner) = (request.repo.host.as_str(), request.repo.owner.as_str());
    if let Some(id) = request.identity {
        let named = everyone
            .iter()
            .find(|i| i.id == id)
            .ok_or(StoreError::UnknownIdentity(id))?;
        return Ok((Some(named), StartBasis::Chosen));
    }
    // Whether an identity covers the repository by naming its owner (`true`) or as the rest of
    // its host (`false`): naming the owner wins, and one already on the
    // list wins over one that would have to join.
    let level = |identity: &Identity| match resolve(std::slice::from_ref(identity), host, owner) {
        CredentialChoice::Covered { exact, .. } => Some(exact),
        _ => None,
    };
    for exact in [true, false] {
        for pool in [list, everyone] {
            if let Some(identity) = pool.iter().find(|i| level(i) == Some(exact)) {
                return Ok((Some(identity), StartBasis::Covers));
            }
        }
    }
    Ok(match everyone.iter().find(|i| i.is_default) {
        Some(default) => (Some(default), StartBasis::Default),
        None => (None, StartBasis::NoIdentity),
    })
}

fn defaults_of(conn: &Connection, user: UserId) -> Result<GitDefaults, StoreError> {
    Ok(conn
        .query_row(
            "SELECT only_push_listed, only_pull_listed FROM git_defaults WHERE user_id = ?1",
            [user.get()],
            |r| {
                Ok(GitDefaults {
                    only_push_listed: r.get(0)?,
                    only_pull_listed: r.get(1)?,
                })
            },
        )
        .optional()?
        .unwrap_or_default())
}

/// One repository's part of a start: the identity joins the list, the row is made.
fn start_in(
    tx: &Transaction<'_>,
    now: u64,
    fx: &mut Vec<Event>,
    ws: &WorkspaceId,
    request: &RepoGitRequest,
) -> Result<GitStart, StoreError> {
    let everyone = all(tx, users::owner_in(tx, ws)?)?;
    let mut list = attached(tx, ws)?;
    let (chosen, basis) = choose(&everyone, &list, request)?;
    let identity = chosen.map(|i| (i.id, i.label.clone()));
    let mut changed = false;
    if let Some(identity) = chosen
        && !list.iter().any(|i| i.id == identity.id)
    {
        list.push(identity.clone());
        check_list(&list)?;
        let ids: Vec<IdentityId> = list.iter().map(|i| i.id).collect();
        write_list(tx, ws, &ids)?;
        changed = true;
    }
    let repo = &request.repo;
    changed |= tx.execute(
        "INSERT INTO workspace_repos (workspace_id, host, owner, repo, pull, push, push_block, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT DO NOTHING",
        params![
            ws.as_str(),
            repo.host.as_str(),
            repo.owner.as_str(),
            repo.repo,
            request.pull,
            request.push,
            request.push_block.map(PushBlock::as_str),
            sql_ts(now)
        ],
    )? > 0;
    if changed {
        fx.push(Event::WorkspaceGitChanged {
            workspace: ws.clone(),
        });
    }
    Ok(GitStart { identity, basis })
}
