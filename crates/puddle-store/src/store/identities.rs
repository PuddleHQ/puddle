// SPDX-License-Identifier: AGPL-3.0-or-later
//! Identities and each workspace's Git settings in the store (`docs/arch/spec/credentials.md`
//! §7 and §8). Credentials are stored as references to their sources; no value passes through.

use std::collections::HashSet;

use puddle_secrets::StoredId;
use puddle_types::{Event, WorkspaceId};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use super::{Store, lock, sql_ts, users};
use crate::error::StoreError;
use crate::identity::{
    Author, CredentialBinding, CredentialChoice, Identity, IdentityDraft, IdentityId, Signing,
    check_attachable, collision, resolve,
};
use crate::users::UserId;
use crate::workspace_git::{
    GitDefaults, GitStart, RepoEntry, RepoGitRequest, RepoRef, StartBasis, SwitchChange,
    SwitchSource, WorkspaceGit,
};

const COLUMNS: &str = "id, label, author_name, author_email, credentials, signing, is_default, \
                       created_at, changed_at";

/// Whether a user has no identity yet.
const NONE_YET: &str = "SELECT count(*) = 0 FROM identities WHERE user_id = ?1";

struct Raw {
    id: i64,
    label: String,
    name: String,
    email: String,
    credentials: String,
    signing: String,
    is_default: bool,
    created_at: i64,
    changed_at: i64,
}

fn raw(row: &rusqlite::Row<'_>) -> rusqlite::Result<Raw> {
    Ok(Raw {
        id: row.get(0)?,
        label: row.get(1)?,
        name: row.get(2)?,
        email: row.get(3)?,
        credentials: row.get(4)?,
        signing: row.get(5)?,
        is_default: row.get(6)?,
        created_at: row.get(7)?,
        changed_at: row.get(8)?,
    })
}

fn identity_of(raw: Raw) -> Result<Identity, StoreError> {
    let corrupt = |reason: String| StoreError::Corrupt {
        table: "identities",
        id: raw.id,
        reason,
    };
    let credentials: Vec<CredentialBinding> =
        serde_json::from_str(&raw.credentials).map_err(|e| corrupt(e.to_string()))?;
    let author = Author::new(&raw.name, &raw.email).map_err(|e| corrupt(e.to_string()))?;
    let signing = match raw.signing.as_str() {
        "none" => Signing::None,
        other => return Err(corrupt(format!("unknown signing {other}"))),
    };
    Ok(Identity {
        id: IdentityId(raw.id),
        label: raw.label,
        author,
        credentials,
        signing,
        is_default: raw.is_default,
        created_at: u64::try_from(raw.created_at).unwrap_or(0),
        changed_at: u64::try_from(raw.changed_at).unwrap_or(0),
    })
}

/// Every identity of `user`, in their order.
fn all(conn: &Connection, user: UserId) -> Result<Vec<Identity>, StoreError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM identities WHERE user_id = ?1 ORDER BY position, id"
    ))?;
    let rows = stmt
        .query_map([user.get()], raw)?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter().map(identity_of).collect()
}

/// Every identity of every user: only for what the install keeps for all of them.
fn everyones(conn: &Connection) -> Result<Vec<Identity>, StoreError> {
    let mut stmt = conn.prepare(&format!("SELECT {COLUMNS} FROM identities ORDER BY id"))?;
    let rows = stmt.query_map([], raw)?.collect::<Result<Vec<_>, _>>()?;
    rows.into_iter().map(identity_of).collect()
}

/// One identity of `user`; another user's is unknown, as one that does not exist.
fn one(conn: &Connection, user: UserId, id: IdentityId) -> Result<Identity, StoreError> {
    let raw = conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM identities WHERE id = ?1 AND user_id = ?2"),
            params![id.0, user.get()],
            raw,
        )
        .optional()?
        .ok_or(StoreError::UnknownIdentity(id))?;
    identity_of(raw)
}

fn attached_ids(conn: &Connection, ws: &WorkspaceId) -> Result<Vec<IdentityId>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT identity_id FROM workspace_identities WHERE workspace_id = ?1 ORDER BY position",
    )?;
    let ids = stmt
        .query_map([ws.as_str()], |r| r.get::<_, i64>(0).map(IdentityId))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ids)
}

fn attached(conn: &Connection, ws: &WorkspaceId) -> Result<Vec<Identity>, StoreError> {
    let user = users::owner_in(conn, ws)?;
    attached_ids(conn, ws)?
        .into_iter()
        .map(|id| one(conn, user, id))
        .collect()
}

fn workspaces_using(conn: &Connection, id: IdentityId) -> Result<Vec<WorkspaceId>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT workspace_id FROM workspace_identities WHERE identity_id = ?1 ORDER BY 1",
    )?;
    let names = stmt
        .query_map([id.0], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(names
        .into_iter()
        .filter_map(|n| WorkspaceId::new(&n).ok())
        .collect())
}

fn write_list(
    tx: &Transaction<'_>,
    ws: &WorkspaceId,
    ids: &[IdentityId],
) -> Result<(), StoreError> {
    tx.execute(
        "DELETE FROM workspace_identities WHERE workspace_id = ?1",
        [ws.as_str()],
    )?;
    for (position, id) in ids.iter().enumerate() {
        tx.execute(
            "INSERT INTO workspace_identities (workspace_id, identity_id, position) VALUES (?1, ?2, ?3)",
            params![ws.as_str(), id.0, i64::try_from(position).unwrap_or(i64::MAX)],
        )?;
    }
    Ok(())
}

/// Refuses a list of identities in which two cover the same place.
fn check_list(list: &[Identity]) -> Result<(), StoreError> {
    for (i, a) in list.iter().enumerate() {
        if let Some(c) = list.iter().skip(i + 1).find_map(|b| collision(a, b)) {
            return Err(StoreError::IdentityCollision(c));
        }
    }
    Ok(())
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

fn label_free(
    tx: &Transaction<'_>,
    user: UserId,
    label: &str,
    except: Option<IdentityId>,
) -> Result<(), StoreError> {
    let taken: Option<i64> = tx
        .query_row(
            "SELECT id FROM identities WHERE label = ?1 AND id IS NOT ?2 AND user_id = ?3",
            params![label, except.map(|i| i.0), user.get()],
            |r| r.get(0),
        )
        .optional()?;
    if taken.is_some() {
        return Err(StoreError::IdentityLabelTaken(label.to_owned()));
    }
    Ok(())
}

fn credentials_json(credentials: &[CredentialBinding]) -> Result<String, StoreError> {
    serde_json::to_string(credentials)
        .map_err(|e| StoreError::IdentityInvalid(format!("credentials can't be stored: {e}")))
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

fn changed_for(fx: &mut Vec<Event>, workspaces: Vec<WorkspaceId>) {
    fx.push(Event::IdentitiesChanged {});
    fx.extend(
        workspaces
            .into_iter()
            .map(|workspace| Event::WorkspaceGitChanged { workspace }),
    );
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
        "INSERT INTO workspace_repos (workspace_id, host, owner, repo, pull, push, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT DO NOTHING",
        params![
            ws.as_str(),
            repo.host.as_str(),
            repo.owner.as_str(),
            repo.repo,
            request.pull,
            request.push,
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

impl Store {
    /// Every id a value in the credential store may still be kept under: the pasted tokens the
    /// identities of every user read and the values of environment secrets. A value under any
    /// other id has nothing that names it.
    ///
    /// # Errors
    /// A database error, or a stored row that doesn't parse: a caller that removes what is not
    /// listed must not go on then.
    pub fn secret_ids_in_use(&self) -> Result<HashSet<StoredId>, StoreError> {
        let conn = lock(&self.conn);
        let mut ids: HashSet<StoredId> = everyones(&conn)?
            .iter()
            .flat_map(|identity| &identity.credentials)
            .filter_map(|binding| binding.stored_id().cloned())
            .collect();
        let mut stmt =
            conn.prepare("SELECT id, secret_id FROM env_vars WHERE secret_id IS NOT NULL")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (row_id, secret_id) = row?;
            ids.insert(StoredId::new(secret_id).map_err(|e| StoreError::Corrupt {
                table: "env_vars",
                id: row_id,
                reason: e.to_string(),
            })?);
        }
        Ok(ids)
    }

    /// Every identity of `user`, in the order they keep them.
    ///
    /// # Errors
    /// A database error, or a stored row that doesn't parse.
    pub fn identities(&self, user: UserId) -> Result<Vec<Identity>, StoreError> {
        all(&lock(&self.conn), user)
    }

    /// One identity of `user`.
    ///
    /// # Errors
    /// [`StoreError::UnknownIdentity`] (also for another user's), or a database error.
    pub fn identity(&self, user: UserId, id: IdentityId) -> Result<Identity, StoreError> {
        one(&lock(&self.conn), user, id)
    }

    /// The default identity of `user`: the one a new workspace of theirs gets when none covers
    /// its URL.
    ///
    /// # Errors
    /// A database error.
    pub fn default_identity(&self, user: UserId) -> Result<Option<Identity>, StoreError> {
        Ok(all(&lock(&self.conn), user)?
            .into_iter()
            .find(|i| i.is_default))
    }

    /// The workspaces an identity is on.
    ///
    /// # Errors
    /// A database error.
    pub fn identity_workspaces(&self, id: IdentityId) -> Result<Vec<WorkspaceId>, StoreError> {
        workspaces_using(&lock(&self.conn), id)
    }

    /// Makes an identity of `user`, last in their order; their first one is their default.
    ///
    /// # Errors
    /// [`StoreError::IdentityInvalid`], [`StoreError::IdentityLabelTaken`], or a database error.
    pub fn create_identity(
        &self,
        user: UserId,
        draft: IdentityDraft,
    ) -> Result<Identity, StoreError> {
        let draft = draft.checked()?;
        let id = self.change(|tx, now, fx| {
            label_free(tx, user, &draft.label, None)?;
            let first: bool = tx.query_row(NONE_YET, [user.get()], |r| r.get(0))?;
            tx.execute(
                "INSERT INTO identities (user_id, label, author_name, author_email, credentials,
                                         position, is_default, created_at, changed_at)
                 VALUES (?7, ?1, ?2, ?3, ?4,
                         (SELECT coalesce(max(position) + 1, 0) FROM identities
                          WHERE user_id = ?7),
                         ?5, ?6, ?6)",
                params![
                    draft.label,
                    draft.author.name,
                    draft.author.email,
                    credentials_json(&draft.credentials)?,
                    first,
                    sql_ts(now),
                    user.get(),
                ],
            )?;
            changed_for(fx, Vec::new());
            Ok(IdentityId(tx.last_insert_rowid()))
        })?;
        tracing::info!(identity = %id, "identity created");
        self.identity(user, id)
    }

    /// Replaces an identity's label, author and credentials. Refused with a collision when the new
    /// coverage meets another identity on a workspace that has both.
    ///
    /// # Errors
    /// [`StoreError::UnknownIdentity`], [`StoreError::IdentityInvalid`],
    /// [`StoreError::IdentityLabelTaken`], [`StoreError::IdentityCollision`], or a database error.
    pub fn update_identity(
        &self,
        user: UserId,
        id: IdentityId,
        draft: IdentityDraft,
    ) -> Result<Identity, StoreError> {
        let draft = draft.checked()?;
        self.change(|tx, now, fx| {
            let before = one(tx, user, id)?;
            label_free(tx, user, &draft.label, Some(id))?;
            let mut after = before.clone();
            after.label.clone_from(&draft.label);
            after.author = draft.author.clone();
            after.credentials.clone_from(&draft.credentials);
            let workspaces = workspaces_using(tx, id)?;
            for ws in &workspaces {
                check_attachable(&attached(tx, ws)?, &after)
                    .map_err(StoreError::IdentityCollision)?;
            }
            tx.execute(
                "UPDATE identities SET label = ?2, author_name = ?3, author_email = ?4,
                        credentials = ?5, changed_at = ?6 WHERE id = ?1",
                params![
                    id.0,
                    draft.label,
                    draft.author.name,
                    draft.author.email,
                    credentials_json(&draft.credentials)?,
                    sql_ts(now),
                ],
            )?;
            changed_for(fx, workspaces);
            Ok(())
        })?;
        self.identity(user, id)
    }

    /// Deletes an identity of `user` and takes it off every workspace; returns the workspaces it
    /// left. The default passes to the first of their identities in their order.
    ///
    /// # Errors
    /// [`StoreError::UnknownIdentity`], or a database error.
    pub fn delete_identity(
        &self,
        user: UserId,
        id: IdentityId,
    ) -> Result<Vec<WorkspaceId>, StoreError> {
        let left = self.change(|tx, _now, fx| {
            let gone = one(tx, user, id)?;
            let workspaces = workspaces_using(tx, id)?;
            tx.execute(
                "DELETE FROM workspace_identities WHERE identity_id = ?1",
                [id.0],
            )?;
            tx.execute("DELETE FROM identities WHERE id = ?1", [id.0])?;
            if gone.is_default {
                tx.execute(
                    "UPDATE identities SET is_default = 1
                     WHERE id = (SELECT id FROM identities WHERE user_id = ?1
                                 ORDER BY position, id LIMIT 1)",
                    [user.get()],
                )?;
            }
            for ws in &workspaces {
                let rest: Vec<IdentityId> = attached_ids(tx, ws)?;
                write_list(tx, ws, &rest)?;
            }
            changed_for(fx, workspaces.clone());
            Ok(workspaces)
        })?;
        tracing::info!(identity = %id, workspaces = left.len(), "identity deleted");
        Ok(left)
    }

    /// Makes `id` the default identity of `user`.
    ///
    /// # Errors
    /// [`StoreError::UnknownIdentity`], or a database error.
    pub fn set_default_identity(
        &self,
        user: UserId,
        id: IdentityId,
    ) -> Result<Identity, StoreError> {
        self.change(|tx, _now, fx| {
            one(tx, user, id)?;
            tx.execute(
                "UPDATE identities SET is_default = 0 WHERE is_default = 1 AND user_id = ?1",
                [user.get()],
            )?;
            tx.execute("UPDATE identities SET is_default = 1 WHERE id = ?1", [id.0])?;
            changed_for(fx, Vec::new());
            Ok(())
        })?;
        self.identity(user, id)
    }

    /// Puts the identities of `user` in a new order; `ids` must name every one of them once.
    ///
    /// # Errors
    /// [`StoreError::IdentityInvalid`] when `ids` is not a permutation of their identities, or a
    /// database error.
    pub fn reorder_identities(
        &self,
        user: UserId,
        ids: &[IdentityId],
    ) -> Result<Vec<Identity>, StoreError> {
        self.change(|tx, _now, fx| {
            let mut have: Vec<IdentityId> = all(tx, user)?.into_iter().map(|i| i.id).collect();
            let mut want = ids.to_vec();
            have.sort();
            want.sort();
            if have != want {
                return Err(StoreError::IdentityInvalid(
                    "the order must list every identity exactly once".into(),
                ));
            }
            for (position, id) in ids.iter().enumerate() {
                tx.execute(
                    "UPDATE identities SET position = ?2 WHERE id = ?1",
                    params![id.0, i64::try_from(position).unwrap_or(i64::MAX)],
                )?;
            }
            changed_for(fx, Vec::new());
            Ok(())
        })?;
        self.identities(user)
    }

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
            "SELECT id, host, owner, repo, pull, push, created_at FROM workspace_repos
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
                    r.get::<_, i64>(6)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let repos = rows
            .into_iter()
            .map(|(id, host, owner, repo, pull, push, at)| {
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

    /// Replaces the workspace's ordered identity list, from its user's identities.
    ///
    /// # Errors
    /// [`StoreError::UnknownIdentity`] (also another user's), [`StoreError::IdentityInvalid`] for a
    /// repeated id,
    /// [`StoreError::IdentityCollision`] naming both when two cover the same place, or a
    /// database error. Nothing changes on an error.
    pub fn set_workspace_identities(
        &self,
        ws: &WorkspaceId,
        ids: &[IdentityId],
    ) -> Result<WorkspaceGit, StoreError> {
        self.change(|tx, _now, fx| {
            let user = users::owner_in(tx, ws)?;
            let mut list = Vec::with_capacity(ids.len());
            for id in ids {
                if list.iter().any(|i: &Identity| i.id == *id) {
                    return Err(StoreError::IdentityInvalid(
                        "an identity is listed twice".into(),
                    ));
                }
                list.push(one(tx, user, *id)?);
            }
            check_list(&list)?;
            write_list(tx, ws, ids)?;
            fx.push(Event::WorkspaceGitChanged {
                workspace: ws.clone(),
            });
            Ok(())
        })?;
        self.workspace_git(ws)
    }

    /// Adds an identity to the workspace, last unless `position` says otherwise.
    ///
    /// # Errors
    /// As [`Store::set_workspace_identities`], and [`StoreError::IdentityAttached`] when it is
    /// already there.
    pub fn attach_identity(
        &self,
        ws: &WorkspaceId,
        id: IdentityId,
        position: Option<usize>,
    ) -> Result<WorkspaceGit, StoreError> {
        let current = {
            let conn = lock(&self.conn);
            let candidate = one(&conn, users::owner_in(&conn, ws)?, id)?;
            let ids = attached_ids(&conn, ws)?;
            if ids.contains(&id) {
                return Err(StoreError::IdentityAttached {
                    label: candidate.label,
                });
            }
            ids
        };
        let mut ids = current;
        ids.insert(position.unwrap_or(ids.len()).min(ids.len()), id);
        self.set_workspace_identities(ws, &ids)
    }

    /// Takes an identity off the workspace.
    ///
    /// # Errors
    /// [`StoreError::UnknownIdentity`] when it is not on the workspace, or a database error.
    pub fn detach_identity(
        &self,
        ws: &WorkspaceId,
        id: IdentityId,
    ) -> Result<WorkspaceGit, StoreError> {
        let mut ids = attached_ids(&lock(&self.conn), ws)?;
        let before = ids.len();
        ids.retain(|i| *i != id);
        if ids.len() == before {
            return Err(StoreError::UnknownIdentity(id));
        }
        self.set_workspace_identities(ws, &ids)
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
        pull: bool,
        push: bool,
    ) -> Result<RepoEntry, StoreError> {
        let id = self.change(|tx, now, fx| {
            let added = tx.execute(
                "INSERT INTO workspace_repos (workspace_id, host, owner, repo, pull, push, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT DO NOTHING",
                params![
                    ws.as_str(),
                    repo.host.as_str(),
                    repo.owner.as_str(),
                    repo.repo,
                    pull,
                    push,
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
        pull: bool,
        push: bool,
    ) -> Result<RepoEntry, StoreError> {
        self.change(|tx, _now, fx| {
            let n = tx.execute(
                "UPDATE workspace_repos SET pull = ?3, push = ?4 WHERE id = ?1 AND workspace_id = ?2",
                params![id, ws.as_str(), pull, push],
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

/// Drops what a deleted workspace held in the Git tables.
pub(super) fn delete_workspace_rows(
    tx: &Transaction<'_>,
    ws: &WorkspaceId,
) -> Result<bool, StoreError> {
    let mut any = 0;
    for table in ["workspace_identities", "workspace_git", "workspace_repos"] {
        any += tx.execute(
            &format!("DELETE FROM {table} WHERE workspace_id = ?1"),
            [ws.as_str()],
        )?;
    }
    Ok(any > 0)
}

#[cfg(test)]
mod tests;
