// SPDX-License-Identifier: GPL-3.0-or-later
//! Environment variables and secrets in the store (`docs/arch/spec/credentials.md` §9). A
//! secret's value never passes through here: only the id it is filed under in the operating
//! system's credential store, the hosts it is for, and each workspace's stand-in for it.

use std::collections::BTreeMap;

use puddle_secrets::StoredId;
use puddle_types::{Event, WorkspaceId};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use super::{Store, lock, sql_ts};
use crate::environment::{
    EnvChange, EnvDraft, EnvEntry, EnvName, EnvRow, EnvScope, EnvSecret, EnvValue,
    MAX_ENTRIES_PER_SCOPE, MAX_SCOPE_BYTES, SecretHost, SecretPlace, StartValue, StartVar,
};
use crate::error::StoreError;

const COLUMNS: &str = "id, workspace_id, name, kind, value, secret_id, hosts, changed_at, place";

/// The same scope in SQL: the global scope is a NULL workspace, which an expression index and the
/// `ifnull` make comparable.
const IN_SCOPE: &str = "ifnull(workspace_id, '') = ?1";

struct Raw {
    id: i64,
    workspace: Option<String>,
    name: String,
    kind: String,
    value: Option<String>,
    secret_id: Option<String>,
    hosts: Option<String>,
    changed_at: i64,
    place: Option<String>,
}

fn raw(row: &rusqlite::Row<'_>) -> rusqlite::Result<Raw> {
    Ok(Raw {
        id: row.get(0)?,
        workspace: row.get(1)?,
        name: row.get(2)?,
        kind: row.get(3)?,
        value: row.get(4)?,
        secret_id: row.get(5)?,
        hosts: row.get(6)?,
        changed_at: row.get(7)?,
        place: row.get(8)?,
    })
}

fn entry_of(raw: Raw) -> Result<EnvEntry, StoreError> {
    let corrupt = |reason: String| StoreError::Corrupt {
        table: "env_vars",
        id: raw.id,
        reason,
    };
    let scope = match &raw.workspace {
        None => EnvScope::Global,
        Some(name) => {
            EnvScope::Workspace(WorkspaceId::new(name).map_err(|e| corrupt(e.to_string()))?)
        }
    };
    let name = EnvName::existing(&raw.name).map_err(|e| corrupt(e.to_string()))?;
    let value = match (raw.kind.as_str(), raw.value, raw.secret_id, raw.hosts) {
        ("plain", Some(value), None, None) => EnvValue::Plain(value),
        ("secret", None, Some(id), Some(hosts)) => {
            let id = StoredId::new(id).map_err(|e| corrupt(e.to_string()))?;
            let hosts: Vec<SecretHost> =
                serde_json::from_str(&hosts).map_err(|e| corrupt(e.to_string()))?;
            let place = SecretPlace::from_column(raw.place.as_deref())
                .map_err(|e| corrupt(e.to_string()))?;
            EnvValue::Secret(EnvSecret { id, hosts, place })
        }
        (kind, ..) => return Err(corrupt(format!("a {kind} variable of the wrong shape"))),
    };
    Ok(EnvEntry {
        id: raw.id,
        scope,
        name,
        value,
        changed_at: u64::try_from(raw.changed_at).unwrap_or(0),
    })
}

fn list(conn: &Connection, scope: &EnvScope) -> Result<Vec<EnvEntry>, StoreError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM env_vars WHERE {IN_SCOPE} ORDER BY name"
    ))?;
    let rows = stmt
        .query_map([scope.column().unwrap_or("")], raw)?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter().map(entry_of).collect()
}

fn find(
    conn: &Connection,
    scope: &EnvScope,
    name: &EnvName,
) -> Result<Option<EnvEntry>, StoreError> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM env_vars WHERE {IN_SCOPE} AND name = ?2"),
        params![scope.column().unwrap_or(""), name.as_str()],
        raw,
    )
    .optional()?
    .map(entry_of)
    .transpose()
}

/// What a scope holds, in the unit the limits count.
fn scope_bytes(entries: &[EnvEntry]) -> usize {
    entries
        .iter()
        .map(|e| {
            e.name.as_str().len()
                + match &e.value {
                    EnvValue::Plain(value) => value.len(),
                    EnvValue::Secret(_) => 0,
                }
        })
        .sum()
}

fn changed(fx: &mut Vec<Event>, scope: &EnvScope) {
    fx.push(match scope {
        EnvScope::Global => Event::GlobalEnvChanged {},
        EnvScope::Workspace(workspace) => Event::WorkspaceEnvChanged {
            workspace: workspace.clone(),
        },
    });
}

fn hosts_json(hosts: &[SecretHost]) -> Result<String, StoreError> {
    serde_json::to_string(hosts)
        .map_err(|e| StoreError::EnvInvalid(format!("hosts can't be stored: {e}")))
}

impl Store {
    /// The variables of one scope, by name.
    ///
    /// # Errors
    /// A database error, or a stored row that doesn't parse.
    pub fn env_entries(&self, scope: &EnvScope) -> Result<Vec<EnvEntry>, StoreError> {
        list(&lock(&self.conn), scope)
    }

    /// One variable of a scope, if set.
    ///
    /// # Errors
    /// A database error, or a stored row that doesn't parse.
    pub fn env_entry(
        &self,
        scope: &EnvScope,
        name: &EnvName,
    ) -> Result<Option<EnvEntry>, StoreError> {
        find(&lock(&self.conn), scope, name)
    }

    /// What `workspace` sees: its own variables and the global ones, each global one marked when
    /// the workspace's own of the same name hides it. By name; a workspace's own entry comes
    /// before the global one it hides.
    ///
    /// # Errors
    /// A database error, or a stored row that doesn't parse.
    pub fn workspace_env(&self, workspace: &WorkspaceId) -> Result<Vec<EnvRow>, StoreError> {
        let conn = lock(&self.conn);
        let own = list(&conn, &EnvScope::Workspace(workspace.clone()))?;
        let global = list(&conn, &EnvScope::Global)?;
        let hidden: Vec<EnvName> = own.iter().map(|e| e.name.clone()).collect();
        let mut rows: Vec<EnvRow> = global
            .into_iter()
            .map(|entry| EnvRow {
                overridden: hidden.contains(&entry.name),
                entry,
            })
            .chain(own.into_iter().map(|entry| EnvRow {
                entry,
                overridden: false,
            }))
            .collect();
        // Stable: within a name the workspace's own entry sorts first.
        rows.sort_by(|a, b| {
            a.entry
                .name
                .cmp(&b.entry.name)
                .then_with(|| a.overridden.cmp(&b.overridden))
        });
        Ok(rows)
    }

    /// Sets `name` in `scope`, replacing what was there.
    ///
    /// A replaced secret keeps its stand-ins when it stays a secret, so a new value (or new
    /// hosts) never changes what a workspace holds; one that becomes a plain variable loses them.
    ///
    /// # Errors
    /// [`StoreError::EnvInvalid`] when the scope would hold more than 256 variables or 256 KiB of
    /// names and values, or a database error. Nothing changes then.
    pub fn set_env(
        &self,
        scope: &EnvScope,
        name: &EnvName,
        draft: EnvDraft,
    ) -> Result<EnvChange, StoreError> {
        self.change(|tx, now, fx| {
            let before = list(tx, scope)?;
            let replaced = before.iter().find(|e| &e.name == name).cloned();
            let (kind, value, secret_id, hosts, place) = match draft {
                EnvDraft::Plain(value) => ("plain", Some(value), None, None, None),
                EnvDraft::Secret { id, hosts, place } => (
                    "secret",
                    None,
                    Some(id),
                    Some(hosts_json(&hosts)?),
                    place.column(),
                ),
            };
            let (value, secret_id) = (value.as_deref(), secret_id.as_ref().map(StoredId::as_str));
            let others: Vec<EnvEntry> = before
                .iter()
                .filter(|e| &e.name != name)
                .cloned()
                .collect();
            if replaced.is_none() && others.len() >= MAX_ENTRIES_PER_SCOPE {
                return Err(StoreError::EnvInvalid(format!(
                    "{scope} already holds {MAX_ENTRIES_PER_SCOPE} variables, the most one can"
                )));
            }
            let added = name.as_str().len() + value.map_or(0, str::len);
            if scope_bytes(&others) + added > MAX_SCOPE_BYTES {
                return Err(StoreError::EnvInvalid(format!(
                    "{scope} would hold more than {} KiB of names and values",
                    MAX_SCOPE_BYTES / 1024
                )));
            }
            let id = if let Some(old) = &replaced {
                tx.execute(
                    "UPDATE env_vars SET kind = ?2, value = ?3, secret_id = ?4, hosts = ?5, changed_at = ?6,
                     place = ?7 WHERE id = ?1",
                    params![old.id, kind, value, secret_id, hosts, sql_ts(now), place],
                )?;
                if kind == "plain" {
                    tx.execute("DELETE FROM env_stand_ins WHERE env_id = ?1", [old.id])?;
                }
                old.id
            } else {
                tx.execute(
                    "INSERT INTO env_vars (workspace_id, name, kind, value, secret_id, hosts, changed_at, place)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![
                        scope.column(),
                        name.as_str(),
                        kind,
                        value,
                        secret_id,
                        hosts,
                        sql_ts(now),
                        place
                    ],
                )?;
                tx.last_insert_rowid()
            };
            changed(fx, scope);
            let entry = find(tx, scope, name)?.ok_or(StoreError::Corrupt {
                table: "env_vars",
                id,
                reason: "a variable that was just written is missing".to_owned(),
            })?;
            Ok(EnvChange { entry, replaced })
        })
    }

    /// Removes `name` from `scope`, with its stand-ins.
    ///
    /// # Errors
    /// [`StoreError::UnknownEnv`] when it isn't set, or a database error. Whoever calls this
    /// removes a removed secret's value from the credential store.
    pub fn delete_env(&self, scope: &EnvScope, name: &EnvName) -> Result<EnvEntry, StoreError> {
        self.change(|tx, _now, fx| {
            let entry = find(tx, scope, name)?.ok_or_else(|| StoreError::UnknownEnv {
                scope: scope.to_string(),
                name: name.to_string(),
            })?;
            tx.execute("DELETE FROM env_vars WHERE id = ?1", [entry.id])?;
            changed(fx, scope);
            Ok(entry)
        })
    }

    /// What `workspace` starts with: the global variables, then its own over them by name. Every
    /// secret gets its stand-in here: the one this workspace already holds for it, else one made
    /// by `mint` (given the secret's name) and kept, so the same stand-in is there at every start.
    ///
    /// # Errors
    /// [`StoreError::EnvInvalid`] when `mint` fails (its message), or a database error.
    pub fn env_for_start(
        &self,
        workspace: &WorkspaceId,
        mint: &mut dyn FnMut(&EnvName) -> Result<String, String>,
    ) -> Result<Vec<StartVar>, StoreError> {
        self.change(|tx, _now, _fx| {
            let mut effective: BTreeMap<EnvName, EnvEntry> = BTreeMap::new();
            for scope in [EnvScope::Global, EnvScope::Workspace(workspace.clone())] {
                for entry in list(tx, &scope)? {
                    effective.insert(entry.name.clone(), entry);
                }
            }
            effective
                .into_values()
                .map(|entry| {
                    let value = match entry.value {
                        EnvValue::Plain(value) => StartValue::Plain(value),
                        EnvValue::Secret(EnvSecret { id, hosts, place }) => StartValue::Secret {
                            stand_in: stand_in(tx, workspace, entry.id, &entry.name, mint)?,
                            id,
                            hosts,
                            place,
                        },
                    };
                    Ok(StartVar {
                        name: entry.name,
                        value,
                    })
                })
                .collect()
        })
    }
}

/// The stand-in `workspace` holds for the secret row `env_id`, made when it has none.
fn stand_in(
    tx: &Transaction<'_>,
    workspace: &WorkspaceId,
    env_id: i64,
    name: &EnvName,
    mint: &mut dyn FnMut(&EnvName) -> Result<String, String>,
) -> Result<String, StoreError> {
    let held: Option<String> = tx
        .query_row(
            "SELECT stand_in FROM env_stand_ins WHERE workspace_id = ?1 AND env_id = ?2",
            params![workspace.as_str(), env_id],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(held) = held {
        return Ok(held);
    }
    let made = mint(name).map_err(StoreError::EnvInvalid)?;
    tx.execute(
        "INSERT INTO env_stand_ins (workspace_id, env_id, stand_in) VALUES (?1, ?2, ?3)",
        params![workspace.as_str(), env_id, made],
    )?;
    Ok(made)
}

/// What a deleted workspace held: its own variables and every stand-in it held. The ids of its own
/// secrets are returned, so their values can leave the credential store.
pub(super) fn delete_workspace_rows(
    tx: &Transaction<'_>,
    workspace: &WorkspaceId,
) -> Result<(Vec<(EnvName, StoredId)>, bool), StoreError> {
    let own = list(tx, &EnvScope::Workspace(workspace.clone()))?;
    let held = tx.execute(
        "DELETE FROM env_stand_ins WHERE workspace_id = ?1",
        [workspace.as_str()],
    )?;
    tx.execute(
        "DELETE FROM env_vars WHERE workspace_id = ?1",
        [workspace.as_str()],
    )?;
    let secrets = own
        .iter()
        .filter_map(|e| Some((e.name.clone(), e.value.as_secret()?.id.clone())))
        .collect();
    Ok((secrets, !own.is_empty() || held > 0))
}

#[cfg(test)]
mod tests;
