// SPDX-License-Identifier: AGPL-3.0-or-later
//! Reading a database that came from a backup file: checking it (restore refuses one it cannot
//! read faithfully), bringing it to this Puddle's schema, and counting what a restore into the
//! present database would add, keep, replace or drop.
//!
//! Nothing here changes the present database. The staged copy is the only file that is written (it
//! is migrated forward), and it belongs to the restore that staged it.

use std::collections::BTreeMap;
use std::path::Path;

use rusqlite::{Connection, OpenFlags};

use crate::error::StoreError;
use crate::schema::{self, SCHEMA_VERSION};

/// Why a staged database was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum DatabaseRefusal {
    /// The file is not a SQLite database, or not one Puddle wrote.
    #[error("puddle.db is not a Puddle database: {0}")]
    NotPuddle(String),
    /// SQLite's integrity check failed.
    #[error("puddle.db fails the database integrity check: {0}")]
    Integrity(String),
    /// A newer Puddle wrote it.
    #[error(
        "made by a newer Puddle, schema {found}; this one reads up to {supported}: update Puddle first"
    )]
    NewerSchema {
        /// The backup's schema version.
        found: u32,
        /// The newest version this build reads.
        supported: u32,
    },
}

/// What [`check_staged_database`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StagedDatabase {
    /// The schema version the file had before it was migrated.
    pub schema_version: u32,
}

/// Checks the database file `path` and migrates it to [`SCHEMA_VERSION`] in place, so a preview and
/// an apply read one schema whatever Puddle made the backup. `path` is a private copy: it is
/// written.
///
/// # Errors
///
/// `Ok(Err(refusal))` when the backup cannot be read faithfully; `Err` when the copy itself fails
/// (a migration step that fails is rolled back and reported here).
pub fn check_staged_database(
    path: &Path,
) -> Result<Result<StagedDatabase, DatabaseRefusal>, StoreError> {
    let Ok(mut conn) = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE) else {
        return Ok(Err(DatabaseRefusal::NotPuddle(
            "it cannot be opened".into(),
        )));
    };
    // `integrity_check` is the first statement that reads the file: a non-database fails here.
    let check: Result<String, _> = conn.query_row("PRAGMA integrity_check", [], |r| r.get(0));
    match check {
        Ok(answer) if answer == "ok" => {}
        Ok(answer) => return Ok(Err(DatabaseRefusal::Integrity(answer))),
        Err(e) => return Ok(Err(DatabaseRefusal::NotPuddle(e.to_string()))),
    }
    let version: u32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version == 0 {
        return Ok(Err(DatabaseRefusal::NotPuddle(
            "it has no schema version".into(),
        )));
    }
    if version > SCHEMA_VERSION {
        return Ok(Err(DatabaseRefusal::NewerSchema {
            found: version,
            supported: SCHEMA_VERSION,
        }));
    }
    conn.pragma_update(None, "foreign_keys", true)?;
    schema::migrate(&mut conn)?;
    Ok(Ok(StagedDatabase {
        schema_version: version,
    }))
}

/// How the entries of one part of a backup compare with the present ones (see the backup page,
/// BK-14). Entries match by natural key.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PartCounts {
    /// Entries in the backup.
    pub in_backup: usize,
    /// In the backup and not here: both modes add them.
    pub add: usize,
    /// In both and different: Add keeps yours, Replace takes the backup's.
    pub differ: usize,
    /// In both and equal: nothing changes.
    pub same: usize,
    /// Here and not in the backup: Add keeps them, Replace drops them.
    pub only_here: usize,
}

impl PartCounts {
    /// Compares two key-to-content maps; `in_backup` is the number of rows, which may exceed the
    /// number of keys when a backup holds the same key twice.
    fn compare(present: &BTreeMap<String, String>, backup: &BTreeMap<String, String>) -> Self {
        let mut counts = Self {
            in_backup: backup.len(),
            ..Self::default()
        };
        for (key, content) in backup {
            match present.get(key) {
                None => counts.add += 1,
                Some(here) if here == content => counts.same += 1,
                Some(_) => counts.differ += 1,
            }
        }
        counts.only_here = present.keys().filter(|k| !backup.contains_key(*k)).count();
        counts
    }
}

/// A secret environment variable of the backup: the restore carries the row, never the value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretEnv {
    /// The workspace it belongs to; `None` for a global one.
    pub workspace_id: Option<String>,
    /// The variable's name.
    pub name: String,
    /// The id the credential store keeps the value under.
    pub secret_id: String,
}

/// What a restore of a staged database would do to the present one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabasePreview {
    /// Rules and request rules.
    pub rules: PartCounts,
    /// Rule sets.
    pub rule_sets: PartCounts,
    /// Git identities.
    pub identities: PartCounts,
    /// Environment variables.
    pub environment: PartCounts,
    /// Users of the backup, matched to this machine's by name (the owner to the owner).
    pub users: PartCounts,
    /// The backup's secret environment variables, for the caller to look up in the credential
    /// store.
    pub secret_envs: Vec<SecretEnv>,
}

/// Maps a user id of `db` to the name used as the match key: the owner is `owner` on both sides,
/// since a restore maps the backup's owner to this machine's.
fn user_key(db: &str) -> String {
    format!(
        "CASE WHEN (SELECT role FROM {db}.users WHERE id = x.user_id) = 'owner' THEN '' \
         ELSE lower((SELECT name FROM {db}.users WHERE id = x.user_id)) END"
    )
}

/// Entries of one part keyed by natural key, each with a signature of what else the entry holds.
type Keyed = BTreeMap<String, String>;

fn rows(conn: &Connection, sql: &str) -> Result<Keyed, StoreError> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// The key and content queries of each part, for the database `db` (`main` or `backup`).
fn queries(db: &str) -> [(&'static str, String); 6] {
    let u = user_key(db);
    [
        (
            "rules",
            format!(
                "SELECT 'r|' || x.scope || '|' || ifnull(x.workspace_id,'') || '|' || \
                 ifnull((SELECT lower(name) FROM {db}.rule_sets s WHERE s.id = x.set_id),'') || '|' || \
                 x.pattern_kind || '|' || x.pattern || '|' || x.effect || '|' || ({u}), \
                 x.enabled || '|' || x.precedence || '|' || ifnull(x.expires_at,'') \
                 FROM {db}.rules x"
            ),
        ),
        (
            "request_rules",
            format!(
                "SELECT 'q|' || x.scope || '|' || ifnull(x.workspace_id,'') || '|' || lower(x.host) \
                 || '|' || x.methods || '|' || x.path || '|' || ({u}), \
                 x.enabled || '|' || x.precedence FROM {db}.request_rules x"
            ),
        ),
        (
            "rule_sets",
            format!(
                "SELECT ({u}) || '|' || lower(x.name), \
                 x.description || '|' || ifnull(x.default_precedence,'') || '|' || ifnull(x.fill,'') \
                 FROM {db}.rule_sets x"
            ),
        ),
        (
            "identities",
            format!(
                "SELECT ({u}) || '|' || lower(x.label), \
                 x.author_name || '|' || x.author_email || '|' || x.credentials \
                 FROM {db}.identities x"
            ),
        ),
        (
            "environment",
            format!(
                "SELECT ({u}) || '|' || ifnull(x.workspace_id,'') || '|' || x.name, \
                 x.kind || '|' || ifnull(x.value,'') || '|' || ifnull(x.hosts,'') \
                 FROM {db}.env_vars x"
            ),
        ),
        (
            "users",
            format!(
                "SELECT CASE WHEN x.role = 'owner' THEN '' ELSE lower(x.name) END, x.role \
                 FROM {db}.users x"
            ),
        ),
    ]
}

/// Compares the staged database `backup` (already migrated by [`check_staged_database`]) with the
/// present database `present`, which is opened read-only and never changed.
///
/// # Errors
///
/// [`StoreError::Database`] when either file cannot be read.
pub fn preview_merge(present: &Path, backup: &Path) -> Result<DatabasePreview, StoreError> {
    let conn = Connection::open_with_flags(present, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.execute(
        "ATTACH DATABASE ?1 AS backup",
        [backup.to_string_lossy().as_ref()],
    )?;
    conn.pragma_update(None, "query_only", true)?;
    let mut maps: BTreeMap<&str, (Keyed, Keyed)> = BTreeMap::new();
    for ((part, here), (_, there)) in queries("main").into_iter().zip(queries("backup")) {
        maps.insert(part, (rows(&conn, &here)?, rows(&conn, &there)?));
    }
    let compare = |parts: &[&str]| {
        let mut here = Keyed::new();
        let mut there = Keyed::new();
        for part in parts {
            if let Some((h, t)) = maps.get(part) {
                here.extend(h.clone());
                there.extend(t.clone());
            }
        }
        PartCounts::compare(&here, &there)
    };
    let mut stmt = conn.prepare(
        "SELECT workspace_id, name, secret_id FROM backup.env_vars \
         WHERE kind = 'secret' ORDER BY workspace_id, name",
    )?;
    let secret_envs = stmt
        .query_map([], |r| {
            Ok(SecretEnv {
                workspace_id: r.get(0)?,
                name: r.get(1)?,
                secret_id: r.get(2)?,
            })
        })?
        .collect::<Result<_, _>>()?;
    Ok(DatabasePreview {
        rules: compare(&["rules", "request_rules"]),
        rule_sets: compare(&["rule_sets"]),
        identities: compare(&["identities"]),
        environment: compare(&["environment"]),
        users: compare(&["users"]),
        secret_envs,
    })
}

#[cfg(test)]
mod tests;
