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
    let version: u32 = scalar(&conn, "PRAGMA user_version")?;
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

/// A pasted token of a Git identity of the backup: the restore carries the reference, never the
/// token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityToken {
    /// The identity's label.
    pub identity: String,
    /// The Git host the token is for.
    pub host: String,
    /// The id the credential store keeps the token under.
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
    /// The pasted tokens of the backup's Git identities, for the caller to look up in the
    /// credential store.
    pub identity_tokens: Vec<IdentityToken>,
}

/// Maps a user id of `db` to the name used as the match key: the owner is `owner` on both sides,
/// since a restore maps the backup's owner to this machine's.
fn user_key(db: &str) -> String {
    format!(
        "CASE WHEN (SELECT role FROM {db}.users WHERE id = x.user_id) = 'owner' THEN '' \
         ELSE lower((SELECT name FROM {db}.users WHERE id = x.user_id)) END"
    )
}

/// The secret environment variables of the backup.
const SECRET_ENVS: &str = "SELECT workspace_id, name, secret_id FROM backup.env_vars \
                           WHERE kind = 'secret' ORDER BY workspace_id, name";

/// The identities of the backup with their credentials, as stored.
const IDENTITY_CREDENTIALS: &str =
    "SELECT label, credentials FROM backup.identities ORDER BY label";

/// The pasted tokens in the credentials column of one identity: the bindings whose source is a
/// stored token. A column that is not the expected JSON names none (the identity is still
/// restored as it is).
fn stored_tokens(label: &str, credentials: &str) -> Vec<IdentityToken> {
    let Ok(serde_json::Value::Array(bindings)) = serde_json::from_str(credentials) else {
        return Vec::new();
    };
    bindings
        .iter()
        .filter_map(|binding| {
            let source = binding.get("source")?;
            (source.get("kind")?.as_str()? == "stored").then(|| IdentityToken {
                identity: label.to_owned(),
                host: binding
                    .get("host")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                secret_id: source
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            })
        })
        .collect()
}

/// Attaches the backup database as `backup`.
fn attach(conn: &Connection, backup: &Path) -> Result<(), StoreError> {
    let path = backup.to_string_lossy();
    conn.execute("ATTACH DATABASE ?1 AS backup", [path.as_ref()])?;
    Ok(())
}

/// The one value of a query that returns one row and one column.
fn scalar<T: rusqlite::types::FromSql>(conn: &Connection, sql: &str) -> Result<T, StoreError> {
    Ok(conn.query_row(sql, [], |r| r.get(0))?)
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
    attach(&conn, backup)?;
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
    let mut stmt = conn.prepare(SECRET_ENVS)?;
    let secret_envs = stmt
        .query_map([], |r| {
            Ok(SecretEnv {
                workspace_id: r.get(0)?,
                name: r.get(1)?,
                secret_id: r.get(2)?,
            })
        })?
        .collect::<Result<_, _>>()?;
    let mut stmt = conn.prepare(IDENTITY_CREDENTIALS)?;
    let identity_tokens = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<Result<Vec<_>, _>>()?
        .iter()
        .flat_map(|(label, credentials)| stored_tokens(label, credentials))
        .collect();
    Ok(DatabasePreview {
        rules: compare(&["rules", "request_rules"]),
        rule_sets: compare(&["rule_sets"]),
        identities: compare(&["identities"]),
        environment: compare(&["environment"]),
        users: compare(&["users"]),
        secret_envs,
        identity_tokens,
    })
}

/// How a restore treats what is already here (backup page, BK-14).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreMode {
    /// Nothing present is changed or removed; what the backup has and this machine lacks is added.
    Add,
    /// Rules, identities, environment rows and the per-workspace rows become the backup's.
    Replace,
}

/// What [`restore_database`] made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoredDatabase {
    /// For each user of the backup, the id it has in the new database: the backup's owner is this
    /// machine's owner, a user of the same name is that user, and any other becomes a new user
    /// without a password. Rows of the backup that name a user follow this map.
    pub user_map: Vec<(i64, i64)>,
    /// This machine's owner, who owns whatever the backup's owner owned.
    pub owner_id: i64,
}

/// Brings the database at `path` to [`SCHEMA_VERSION`] in place. A database from a newer Puddle is
/// an error, as at start.
///
/// # Errors
///
/// [`StoreError::SchemaTooNew`] or the SQLite error of the migration (rolled back).
pub fn migrate_database(path: &Path) -> Result<(), StoreError> {
    let mut conn = Connection::open(path)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.pragma_update(None, "foreign_keys", true)?;
    schema::migrate(&mut conn)?;
    Ok(())
}

/// A table of the database and how a restore matches its rows.
struct Table {
    name: &'static str,
    /// The columns that identify a row (its natural key); `NOCASE` ones compare case-insensitively.
    key: &'static [&'static str],
    /// Columns a restore never copies (row ids, which the new database assigns, and references to
    /// rows that are never restored).
    skip: &'static [&'static str],
}

const fn table(name: &'static str, key: &'static [&'static str]) -> Table {
    Table {
        name,
        key,
        skip: &["id"],
    }
}

/// The tables a restore carries, parents before children. The audit log, pending requests,
/// sign-in state and the data derived from what a machine saw are not here (see
/// [`restore_database`]).
fn tables() -> [Table; 21] {
    [
        table("rule_sets", &["user_id", "name NOCASE"]),
        Table {
            skip: &["id", "source_pending_id"],
            ..table(
                "rules",
                &[
                    "user_id",
                    "scope",
                    "workspace_id",
                    "set_id",
                    "pattern_kind",
                    "pattern",
                    "effect",
                ],
            )
        },
        table(
            "request_rules",
            &[
                "user_id",
                "scope",
                "workspace_id",
                "host",
                "methods",
                "path",
            ],
        ),
        table(
            "rule_set_switches",
            &["user_id", "rule_set", "workspace_id"],
        ),
        table(
            "builtin_entry_switches",
            &["user_id", "rule_set", "pattern"],
        ),
        table("env_vars", &["user_id", "workspace_id", "name"]),
        table("env_stand_ins", &["workspace_id", "env_id"]),
        table("identities", &["user_id", "label NOCASE"]),
        table("workspace_identities", &["workspace_id", "identity_id"]),
        table("git_defaults", &["user_id"]),
        table("workspace_git", &["workspace_id"]),
        table("workspace_interception", &["workspace_id"]),
        table("workspace_names", &["workspace_id"]),
        table("workspace_owners", &["workspace_id"]),
        table(
            "workspace_repos",
            &["workspace_id", "host", "owner", "repo"],
        ),
        table("shell_init", &["workspace_id"]),
        table("pinned_ports", &["workspace_id", "guest_port"]),
        table("never_decrypt", &["workspace_id", "pattern"]),
        table("never_decrypt_global", &["pattern"]),
        table("ssh_agent_hosts", &["workspace_id", "host"]),
        table("interception_settings", &["id"]),
    ]
}

/// The table names a Replace clears, children first.
const CLEAR_ORDER: [&str; 21] = [
    "workspace_identities",
    "env_stand_ins",
    "rules",
    "rule_set_switches",
    "builtin_entry_switches",
    "request_rules",
    "env_vars",
    "identities",
    "rule_sets",
    "git_defaults",
    "workspace_git",
    "workspace_interception",
    "workspace_names",
    "workspace_owners",
    "workspace_repos",
    "shell_init",
    "pinned_ports",
    "never_decrypt",
    "never_decrypt_global",
    "ssh_agent_hosts",
    "interception_settings",
];

fn columns(conn: &Connection, schema: &str, table: &str) -> Result<Vec<String>, StoreError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT name FROM pragma_table_info('{table}', '{schema}') ORDER BY cid"
    ))?;
    let names = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<_, _>>()?;
    Ok(names)
}

/// The expression for column `col` of a backup row `b`: ids of other restored rows and users go
/// through the maps; a user id of a row that names a user the backup lacks stays missing.
fn value_of(table: &str, col: &str) -> String {
    match (table, col) {
        (_, "user_id") => "(SELECT p_id FROM temp.umap WHERE b_id = b.user_id)".into(),
        ("rules", "set_id") => "(SELECT p_id FROM temp.smap WHERE b_id = b.set_id)".into(),
        ("env_stand_ins", "env_id") => "(SELECT p_id FROM temp.emap WHERE b_id = b.env_id)".into(),
        ("identities", "is_default") => "CASE WHEN b.is_default = 1 AND EXISTS \
             (SELECT 1 FROM main.identities d WHERE d.is_default = 1 \
              AND d.user_id = (SELECT p_id FROM temp.umap WHERE b_id = b.user_id)) \
             THEN 0 ELSE b.is_default END"
            .into(),
        ("workspace_identities", "identity_id") => {
            "(SELECT p_id FROM temp.imap WHERE b_id = b.identity_id)".into()
        }
        _ => format!("b.{col}"),
    }
}

/// Copies the rows of `t` that the present side lacks (all of them after a clear).
fn copy_rows(conn: &Connection, t: &Table) -> Result<(), StoreError> {
    let main = columns(conn, "main", t.name)?;
    let backup = columns(conn, "backup", t.name)?;
    let cols: Vec<&String> = main
        .iter()
        .filter(|c| backup.contains(c) && !t.skip.contains(&c.as_str()))
        .collect();
    if cols.is_empty() {
        return Ok(());
    }
    let list = cols
        .iter()
        .map(|c| c.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let values = cols
        .iter()
        .map(|c| value_of(t.name, c))
        .collect::<Vec<_>>()
        .join(", ");
    let same = t
        .key
        .iter()
        .map(|k| {
            let (col, nocase) = k
                .strip_suffix(" NOCASE")
                .map_or((*k, ""), |c| (c, " COLLATE NOCASE"));
            format!("m.{col} IS {}{nocase}", value_of(t.name, col))
        })
        .collect::<Vec<_>>()
        .join(" AND ");
    let sql = format!(
        "INSERT OR IGNORE INTO main.{name} ({list}) SELECT {values} FROM backup.{name} b \
         WHERE NOT EXISTS (SELECT 1 FROM main.{name} m WHERE {same})",
        name = t.name
    );
    conn.execute(&sql, [])?;
    Ok(())
}

/// How a row of the backup and one here are the same row, for the id maps.
const SAME_SET: &str = concat!(
    "m.user_id = (SELECT p_id FROM temp.umap WHERE b_id = b.user_id)",
    " AND m.name = b.name COLLATE NOCASE"
);
const SAME_ENV: &str = concat!(
    "m.user_id = (SELECT p_id FROM temp.umap WHERE b_id = b.user_id)",
    " AND ifnull(m.workspace_id, '') = ifnull(b.workspace_id, '') AND m.name = b.name"
);
const SAME_IDENTITY: &str = concat!(
    "m.user_id = (SELECT p_id FROM temp.umap WHERE b_id = b.user_id)",
    " AND m.label = b.label COLLATE NOCASE"
);

/// Builds `temp.<map>` from the rows of `t` that now exist on both sides, matched by their key.
fn map_rows(conn: &Connection, map: &str, table: &str, key: &str) -> Result<(), StoreError> {
    conn.execute_batch(&format!(
        "DROP TABLE IF EXISTS temp.{map};
         CREATE TEMP TABLE {map} AS SELECT b.id AS b_id, m.id AS p_id \
           FROM backup.{table} b JOIN main.{table} m ON {key};"
    ))?;
    Ok(())
}

/// The id mapping of the users: the owner to the owner, the same name to the same user, anyone
/// else added as a user with no password.
fn map_users(conn: &Connection) -> Result<(), StoreError> {
    conn.execute_batch(
        "INSERT INTO main.users (role, name, created_at) \
           SELECT 'user', b.name, b.created_at FROM backup.users b \
           WHERE b.role = 'user' AND NOT EXISTS (SELECT 1 FROM main.users m WHERE m.name = b.name COLLATE NOCASE);
         DROP TABLE IF EXISTS temp.umap;
         CREATE TEMP TABLE umap AS SELECT b.id AS b_id, \
           CASE WHEN b.role = 'owner' THEN (SELECT id FROM main.users WHERE role = 'owner') \
                ELSE (SELECT id FROM main.users m WHERE m.name = b.name COLLATE NOCASE) END AS p_id \
           FROM backup.users b;",
    )?;
    Ok(())
}

/// Makes the database a restore leaves in place: a copy of the present database at `present`
/// (migrated to this schema first) with the staged backup `backup` (already checked and migrated
/// by [`check_staged_database`]) merged in, written to the new file `dest`.
///
/// What is carried: rules and request rules, rule sets and their switches, environment variables,
/// git identities, and the per-workspace rows (git, interception, repositories, shell init, pinned
/// ports, never-decrypt lists, SSH agent hosts, owners). People follow [`RestoredDatabase::user_map`]:
/// this machine's users, their passwords and devices are never touched. Never carried: pending
/// requests (they belong to connections that are gone), sign-in state, and what a machine learned
/// by watching. The audit log is loaded only when this machine's is empty. In [`RestoreMode::Add`]
/// a row is added only when its natural key is missing here; in [`RestoreMode::Replace`] the
/// carried tables are cleared first.
///
/// Neither input is changed beyond the migration of `present`. `dest` is checked (integrity and
/// foreign keys) before this returns.
///
/// # Errors
///
/// [`StoreError`] when a file cannot be read or written, a statement fails (the new file is
/// removed) or the result fails its checks.
pub fn restore_database(
    present: &Path,
    backup: &Path,
    dest: &Path,
    mode: RestoreMode,
) -> Result<RestoredDatabase, StoreError> {
    migrate_database(present)?;
    {
        let source = Connection::open_with_flags(present, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        source.busy_timeout(std::time::Duration::from_secs(5))?;
        source.execute("VACUUM INTO ?1", [dest.to_string_lossy().as_ref()])?;
    }
    let result = merge_into(dest, backup, mode);
    if result.is_err() {
        drop(std::fs::remove_file(dest));
    }
    result
}

fn merge_into(
    dest: &Path,
    backup: &Path,
    mode: RestoreMode,
) -> Result<RestoredDatabase, StoreError> {
    let mut conn = Connection::open_with_flags(dest, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    attach(&conn, backup)?;
    let tx = conn.transaction()?;
    map_users(&tx)?;
    if mode == RestoreMode::Replace {
        for name in CLEAR_ORDER {
            tx.execute(&format!("DELETE FROM main.{name}"), [])?;
        }
    }
    let carried = tables();
    // The maps are built after the table they map is merged and before the tables that use them.
    for t in &carried {
        copy_rows(&tx, t)?;
        match t.name {
            "rule_sets" => map_rows(&tx, "smap", "rule_sets", SAME_SET)?,
            "env_vars" => map_rows(&tx, "emap", "env_vars", SAME_ENV)?,
            "identities" => map_rows(&tx, "imap", "identities", SAME_IDENTITY)?,
            _ => {}
        }
    }
    let log_is_empty: bool = scalar(&tx, "SELECT NOT EXISTS (SELECT 1 FROM main.audit)")?;
    if log_is_empty {
        copy_audit(&tx)?;
    }
    let user_map = {
        let mut stmt = tx.prepare("SELECT b_id, p_id FROM temp.umap ORDER BY b_id")?;
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<Vec<(i64, i64)>, _>>()?
    };
    let broken: i64 = scalar(&tx, "SELECT count(*) FROM pragma_foreign_key_check")?;
    if broken > 0 {
        return Err(StoreError::Database(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY),
            Some(format!(
                "the restored data left {broken} rows without their parent row"
            )),
        )));
    }
    let owner_id: i64 = scalar(&tx, "SELECT id FROM main.users WHERE role = 'owner'")?;
    tx.commit()?;
    conn.execute("DETACH DATABASE backup", [])?;
    let check: String = scalar(&conn, "PRAGMA integrity_check")?;
    verdict(&check)?;
    Ok(RestoredDatabase { user_map, owner_id })
}

fn copy_audit(conn: &Connection) -> Result<(), StoreError> {
    let main = columns(conn, "main", "audit")?;
    let backup = columns(conn, "backup", "audit")?;
    let cols: Vec<&String> = main
        .iter()
        .filter(|c| backup.contains(c) && *c != "id")
        .collect();
    let list = cols
        .iter()
        .map(|c| c.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let values = cols
        .iter()
        .map(|c| value_of("audit", c))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "INSERT INTO main.audit ({list}) SELECT {values} FROM backup.audit b ORDER BY b.id"
    );
    conn.execute(&sql, [])?;
    Ok(())
}

fn verdict(check: &str) -> Result<(), StoreError> {
    if check == "ok" {
        return Ok(());
    }
    Err(StoreError::Database(rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT),
        Some(format!(
            "the restored database failed its integrity check: {check}"
        )),
    )))
}

#[cfg(test)]
mod tests;
