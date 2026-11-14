// SPDX-License-Identifier: AGPL-3.0-or-later
//! Versioned schema migrations, tracked in SQLite's `user_version`.
//!
//! Migrations only ever get appended. Each runs in its own transaction together with the
//! version bump, so a crash leaves the database at the previous version, never in between.

use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::error::StoreError;

/// Schema migrations; entry `n` takes the database from version `n` to `n + 1`.
const MIGRATIONS: &[&str] = &[
    V1, V2, V3, V4, V5, V6, V7, V8, V9, V10, V11, V12, V13, V14, V15, V16, V17, V18, V19, V20, V21,
    V22, V23, V24, V25,
];

/// The schema version this build writes.
pub const SCHEMA_VERSION: u32 = 25;

const V1: &str = r"
CREATE TABLE rules (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    scope             TEXT    NOT NULL CHECK (scope IN ('global', 'sandbox')),
    sandbox_id        TEXT,
    pattern_kind      TEXT    NOT NULL CHECK (pattern_kind IN ('exact', 'suffix')),
    pattern           TEXT    NOT NULL,
    effect            TEXT    NOT NULL CHECK (effect IN ('allow', 'deny')),
    expires_at        INTEGER,
    created_at        INTEGER NOT NULL,
    created_by        TEXT    NOT NULL CHECK (created_by IN ('cli', 'ui', 'api')),
    source_pending_id INTEGER,
    CHECK ((scope = 'global') = (sandbox_id IS NULL))
) STRICT;
CREATE INDEX rules_sandbox ON rules (sandbox_id);
CREATE INDEX rules_expiry ON rules (expires_at) WHERE expires_at IS NOT NULL;

CREATE TABLE pending (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    sandbox_id TEXT    NOT NULL,
    host       TEXT    NOT NULL,
    port       INTEGER NOT NULL CHECK (port BETWEEN 0 AND 65535),
    first_seen INTEGER NOT NULL,
    last_seen  INTEGER NOT NULL,
    attempts   INTEGER NOT NULL DEFAULT 1,
    state      TEXT    NOT NULL DEFAULT 'requested'
               CHECK (state IN ('requested', 'allowed', 'denied', 'expired')),
    decided_at INTEGER,
    decided_by TEXT CHECK (decided_by IN ('cli', 'ui', 'api', 'system')),
    rule_id    INTEGER
) STRICT;
-- R-11: at most one open row per (sandbox, host, port).
CREATE UNIQUE INDEX pending_open_key ON pending (sandbox_id, host, port)
    WHERE state = 'requested';
CREATE INDEX pending_state_seen ON pending (state, last_seen);
-- R-12: a row's identity never changes, and the end states are final.
CREATE TRIGGER pending_identity_immutable
BEFORE UPDATE OF id, sandbox_id, host, port, first_seen ON pending
WHEN NEW.id IS NOT OLD.id OR NEW.sandbox_id IS NOT OLD.sandbox_id OR NEW.host IS NOT OLD.host
  OR NEW.port IS NOT OLD.port OR NEW.first_seen IS NOT OLD.first_seen
BEGIN
    SELECT RAISE(ABORT, 'pending row identity is immutable');
END;
CREATE TRIGGER pending_end_state_final
BEFORE UPDATE OF state ON pending
WHEN OLD.state <> 'requested' AND NEW.state IS NOT OLD.state
BEGIN
    SELECT RAISE(ABORT, 'pending row end state is final');
END;
CREATE TRIGGER pending_no_delete
BEFORE DELETE ON pending
BEGIN
    SELECT RAISE(ABORT, 'pending rows are never deleted');
END;

CREATE TABLE audit (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    ts         INTEGER NOT NULL,
    type       TEXT    NOT NULL,
    sandbox_id TEXT,
    line       TEXT    NOT NULL CHECK (length(CAST(line AS BLOB)) <= 4096)
) STRICT;
CREATE INDEX audit_sandbox ON audit (sandbox_id, id);
-- Running total of line bytes, so the cap check (R-26) doesn't scan the table.
CREATE TABLE audit_size (
    id    INTEGER PRIMARY KEY CHECK (id = 1),
    bytes INTEGER NOT NULL
) STRICT;
INSERT INTO audit_size (id, bytes) VALUES (1, 0);
CREATE TRIGGER audit_size_insert AFTER INSERT ON audit
BEGIN
    UPDATE audit_size SET bytes = bytes + length(CAST(NEW.line AS BLOB)) WHERE id = 1;
END;
CREATE TRIGGER audit_size_delete AFTER DELETE ON audit
BEGIN
    UPDATE audit_size SET bytes = bytes - length(CAST(OLD.line AS BLOB)) WHERE id = 1;
END;
";

/// The audit filters: `host` and `outcome` become columns so the API's filters never
/// parse JSON, and the filtered scans are indexed. Existing rows are backfilled from their
/// lines; `AuditRecord::host` and `AuditRecord::outcome` must give the same values (tested).
const V2: &str = r"
ALTER TABLE audit ADD COLUMN host TEXT;
ALTER TABLE audit ADD COLUMN outcome TEXT;
UPDATE audit SET
    host = lower(CASE type
        WHEN 'connection' THEN json_extract(line, '$.host')
        WHEN 'pending_created' THEN json_extract(line, '$.pending.host')
        WHEN 'pending_decided' THEN json_extract(line, '$.pending.host')
        WHEN 'pending_expired' THEN json_extract(line, '$.pending.host')
        WHEN 'rule_created' THEN json_extract(line, '$.rule.pattern')
        WHEN 'rule_updated' THEN json_extract(line, '$.rule.pattern')
        WHEN 'rule_deleted' THEN json_extract(line, '$.rule.pattern')
        WHEN 'rule_expired' THEN json_extract(line, '$.rule.pattern')
    END),
    outcome = CASE type
        WHEN 'connection' THEN json_extract(line, '$.decision')
        WHEN 'pending_created' THEN 'pending'
        WHEN 'pending_decided' THEN CASE json_extract(line, '$.pending.state')
            WHEN 'allowed' THEN 'allow'
            WHEN 'denied' THEN 'deny'
        END
        WHEN 'pending_expired' THEN 'expired'
    END;
CREATE INDEX audit_type ON audit (type, id);
CREATE INDEX audit_outcome ON audit (outcome, id) WHERE outcome IS NOT NULL;
CREATE INDEX audit_ts ON audit (ts);
";

/// Rule sets (`docs/spec/rules.md` §7). `rules` is rebuilt because SQLite can't change a
/// `CHECK`: a rule's scope may now be `set`, with the set in `set_id`. The `AUTOINCREMENT`
/// high-water mark is carried over, so a deleted rule's id is still never reused.
const V3: &str = r"
CREATE TABLE rule_sets (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    name        TEXT    NOT NULL CHECK (length(name) BETWEEN 1 AND 64),
    description TEXT    NOT NULL DEFAULT '' CHECK (length(description) <= 500),
    created_at  INTEGER NOT NULL,
    created_by  TEXT    NOT NULL CHECK (created_by IN ('cli', 'ui', 'api'))
) STRICT;
CREATE UNIQUE INDEX rule_sets_name ON rule_sets (name COLLATE NOCASE);

CREATE TABLE rules_v3 (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    scope             TEXT    NOT NULL CHECK (scope IN ('global', 'sandbox', 'set')),
    sandbox_id        TEXT,
    set_id            INTEGER REFERENCES rule_sets (id),
    pattern_kind      TEXT    NOT NULL CHECK (pattern_kind IN ('exact', 'suffix')),
    pattern           TEXT    NOT NULL,
    effect            TEXT    NOT NULL CHECK (effect IN ('allow', 'deny')),
    expires_at        INTEGER,
    created_at        INTEGER NOT NULL,
    created_by        TEXT    NOT NULL CHECK (created_by IN ('cli', 'ui', 'api')),
    source_pending_id INTEGER,
    CHECK ((scope = 'sandbox') = (sandbox_id IS NOT NULL)),
    CHECK ((scope = 'set') = (set_id IS NOT NULL))
) STRICT;
INSERT INTO rules_v3 (id, scope, sandbox_id, set_id, pattern_kind, pattern, effect, expires_at,
                      created_at, created_by, source_pending_id)
    SELECT id, scope, sandbox_id, NULL, pattern_kind, pattern, effect, expires_at,
           created_at, created_by, source_pending_id
    FROM rules;
DELETE FROM sqlite_sequence WHERE name = 'rules_v3';
INSERT INTO sqlite_sequence (name, seq) SELECT 'rules_v3', seq FROM sqlite_sequence WHERE name = 'rules';
DROP TABLE rules;
ALTER TABLE rules_v3 RENAME TO rules;
CREATE INDEX rules_sandbox ON rules (sandbox_id);
CREATE INDEX rules_expiry ON rules (expires_at) WHERE expires_at IS NOT NULL;
CREATE INDEX rules_set ON rules (set_id) WHERE set_id IS NOT NULL;

-- The set whose entry decided a row, when one did (`builtin:<slug>`, `user:<id>`, `system`).
ALTER TABLE pending ADD COLUMN rule_set TEXT;

-- On/off per set: one row for every sandbox (sandbox_id NULL) and one per sandbox override.
-- No row means the next level decides: the sandbox's, then every sandbox's, then the set's default.
CREATE TABLE rule_set_switches (
    rule_set   TEXT    NOT NULL,
    sandbox_id TEXT,
    enabled    INTEGER NOT NULL CHECK (enabled IN (0, 1)),
    changed_at INTEGER NOT NULL
) STRICT;
CREATE UNIQUE INDEX rule_set_switches_key ON rule_set_switches (rule_set, ifnull(sandbox_id, ''));

-- Why Puddle allows its System managed hosts, as last derived from the user's setup, so a
-- restart with the same setup changes (and records) nothing.
CREATE TABLE system_reasons (
    sandbox_id TEXT,
    reason     TEXT NOT NULL
) STRICT;
CREATE UNIQUE INDEX system_reasons_key ON system_reasons (ifnull(sandbox_id, ''), reason);

-- The built-in sets' entries as this database last saw them, to record what an update changed.
CREATE TABLE builtin_sets_seen (
    slug       TEXT PRIMARY KEY,
    entries    TEXT    NOT NULL,
    changed_at INTEGER
) STRICT;
";

/// The rules' owner is a workspace, not a sandbox: `sandbox_id` becomes `workspace_id` in every
/// table that has it (SQLite rewrites the triggers and indexes that name it), and a rule's
/// `scope` value `sandbox` becomes `workspace`. `rules` is rebuilt because SQLite can't change
/// a `CHECK`; the `AUTOINCREMENT` high-water mark is carried over as in V3. The stored audit
/// `line`s are not rewritten (the log is append-only and its lines are capped): the reader
/// accepts the old key names.
const V4: &str = r"
ALTER TABLE pending RENAME COLUMN sandbox_id TO workspace_id;
ALTER TABLE audit RENAME COLUMN sandbox_id TO workspace_id;
ALTER TABLE rule_set_switches RENAME COLUMN sandbox_id TO workspace_id;
ALTER TABLE system_reasons RENAME COLUMN sandbox_id TO workspace_id;
DROP INDEX audit_sandbox;
CREATE INDEX audit_workspace ON audit (workspace_id, id);

CREATE TABLE rules_v4 (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    scope             TEXT    NOT NULL CHECK (scope IN ('global', 'workspace', 'set')),
    workspace_id      TEXT,
    set_id            INTEGER REFERENCES rule_sets (id),
    pattern_kind      TEXT    NOT NULL CHECK (pattern_kind IN ('exact', 'suffix')),
    pattern           TEXT    NOT NULL,
    effect            TEXT    NOT NULL CHECK (effect IN ('allow', 'deny')),
    expires_at        INTEGER,
    created_at        INTEGER NOT NULL,
    created_by        TEXT    NOT NULL CHECK (created_by IN ('cli', 'ui', 'api')),
    source_pending_id INTEGER,
    CHECK ((scope = 'workspace') = (workspace_id IS NOT NULL)),
    CHECK ((scope = 'set') = (set_id IS NOT NULL))
) STRICT;
INSERT INTO rules_v4 (id, scope, workspace_id, set_id, pattern_kind, pattern, effect, expires_at,
                      created_at, created_by, source_pending_id)
    SELECT id, CASE scope WHEN 'sandbox' THEN 'workspace' ELSE scope END, sandbox_id, set_id,
           pattern_kind, pattern, effect, expires_at, created_at, created_by, source_pending_id
    FROM rules;
DELETE FROM sqlite_sequence WHERE name = 'rules_v4';
INSERT INTO sqlite_sequence (name, seq) SELECT 'rules_v4', seq FROM sqlite_sequence WHERE name = 'rules';
DROP TABLE rules;
ALTER TABLE rules_v4 RENAME TO rules;
CREATE INDEX rules_workspace ON rules (workspace_id);
CREATE INDEX rules_expiry ON rules (expires_at) WHERE expires_at IS NOT NULL;
CREATE INDEX rules_set ON rules (set_id) WHERE set_id IS NOT NULL;
";

const V5: &str = r"
-- Git identities: an author and credential references (never a secret), several per workspace.
CREATE TABLE identities (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    label       TEXT    NOT NULL COLLATE NOCASE UNIQUE,
    author_name TEXT    NOT NULL,
    author_email TEXT   NOT NULL,
    credentials TEXT    NOT NULL,
    signing     TEXT    NOT NULL DEFAULT 'none' CHECK (signing IN ('none')),
    position    INTEGER NOT NULL,
    is_default  INTEGER NOT NULL DEFAULT 0 CHECK (is_default IN (0, 1)),
    created_at  INTEGER NOT NULL,
    changed_at  INTEGER NOT NULL
) STRICT;
CREATE UNIQUE INDEX identities_one_default ON identities (is_default) WHERE is_default = 1;

-- The identities a workspace has, in its order.
CREATE TABLE workspace_identities (
    workspace_id TEXT    NOT NULL,
    identity_id  INTEGER NOT NULL REFERENCES identities (id) ON DELETE CASCADE,
    position     INTEGER NOT NULL,
    PRIMARY KEY (workspace_id, identity_id)
) STRICT;
CREATE UNIQUE INDEX workspace_identities_order ON workspace_identities (workspace_id, position);

-- The two switches; no row means the defaults (push list on, pull list off).
CREATE TABLE workspace_git (
    workspace_id     TEXT PRIMARY KEY,
    only_push_listed INTEGER NOT NULL DEFAULT 1 CHECK (only_push_listed IN (0, 1)),
    only_pull_listed INTEGER NOT NULL DEFAULT 0 CHECK (only_pull_listed IN (0, 1))
) STRICT;

-- The repository table: one row per repository with its Pull and Push toggles.
CREATE TABLE workspace_repos (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    workspace_id TEXT    NOT NULL,
    host         TEXT    NOT NULL,
    owner        TEXT    NOT NULL,
    repo         TEXT    NOT NULL,
    pull         INTEGER NOT NULL CHECK (pull IN (0, 1)),
    push         INTEGER NOT NULL CHECK (push IN (0, 1)),
    created_at   INTEGER NOT NULL
) STRICT;
CREATE UNIQUE INDEX workspace_repos_key ON workspace_repos (workspace_id, host, owner, repo);
";

const V6: &str = r"
-- Environment variables and secrets: one row per name in a scope (workspace_id NULL = every
-- workspace). A plain variable keeps its value here. A secret keeps only `secret_id`, the id of
-- its value in the operating system's credential store, and the hosts it may be sent to (a JSON
-- array): its value is never in this database.
CREATE TABLE env_vars (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    workspace_id TEXT,
    name         TEXT    NOT NULL,
    kind         TEXT    NOT NULL CHECK (kind IN ('plain', 'secret')),
    value        TEXT,
    secret_id    TEXT,
    hosts        TEXT,
    changed_at   INTEGER NOT NULL,
    CHECK ((kind = 'plain') = (value IS NOT NULL)),
    CHECK ((kind = 'secret') = (secret_id IS NOT NULL)),
    CHECK ((kind = 'secret') = (hosts IS NOT NULL))
) STRICT;
CREATE UNIQUE INDEX env_vars_key ON env_vars (ifnull(workspace_id, ''), name);

-- The stand-in a workspace holds for a secret, made the first time it starts with it and kept, so
-- a file the workspace wrote with it keeps working after a restart. A stand-in is worthless
-- without the secret's hosts; the real value is not here.
CREATE TABLE env_stand_ins (
    workspace_id TEXT    NOT NULL,
    env_id       INTEGER NOT NULL REFERENCES env_vars (id) ON DELETE CASCADE,
    stand_in     TEXT    NOT NULL,
    PRIMARY KEY (workspace_id, env_id)
) STRICT;
";

const V7: &str = r"
-- The two switches become overrides: NULL follows the default for new workspaces (below). Rows
-- written before this stay as explicit values.
CREATE TABLE workspace_git_v7 (
    workspace_id     TEXT PRIMARY KEY,
    only_push_listed INTEGER CHECK (only_push_listed IN (0, 1)),
    only_pull_listed INTEGER CHECK (only_pull_listed IN (0, 1))
) STRICT;
INSERT INTO workspace_git_v7 SELECT workspace_id, only_push_listed, only_pull_listed FROM workspace_git;
DROP TABLE workspace_git;
ALTER TABLE workspace_git_v7 RENAME TO workspace_git;

-- The default of the two switches for every workspace that has not set its own: one row, absent
-- until the user changes it (then push list on, pull list off).
CREATE TABLE git_defaults (
    id               INTEGER PRIMARY KEY CHECK (id = 1),
    only_push_listed INTEGER NOT NULL CHECK (only_push_listed IN (0, 1)),
    only_pull_listed INTEGER NOT NULL CHECK (only_pull_listed IN (0, 1))
) STRICT;
";

const V8: &str = r"
-- The shell init of a workspace: a script its login shells source, written by the user in the UI.
-- This row is the only source of truth; the workspace sees a read-only copy of it. A workspace
-- with no script has no row.
CREATE TABLE shell_init (
    workspace_id TEXT PRIMARY KEY,
    content      TEXT    NOT NULL CHECK (content <> ''),
    changed_at   INTEGER NOT NULL
) STRICT;
";

const V9: &str = r"
-- Pinned port forwards: a port in a workspace that comes back on the same port of this computer
-- every time the workspace starts. One row per workspace and guest port; the host port is the one
-- the user chose (or the guest's own number). Rows go with the workspace.
CREATE TABLE pinned_ports (
    workspace_id TEXT    NOT NULL,
    guest_port   INTEGER NOT NULL CHECK (guest_port BETWEEN 1 AND 65535),
    host_port    INTEGER NOT NULL CHECK (host_port BETWEEN 1 AND 65535),
    PRIMARY KEY (workspace_id, guest_port)
) STRICT;
";

const V10: &str = r"
-- Decrypting every host: a per-workspace switch (off unless a row says on) and the names that are
-- never decrypted while it is on, for one workspace or for every workspace. Rows go with the
-- workspace; the global list stays.
CREATE TABLE workspace_interception (
    workspace_id TEXT    NOT NULL PRIMARY KEY,
    decrypt_all  INTEGER NOT NULL CHECK (decrypt_all IN (0, 1))
) STRICT;
CREATE TABLE never_decrypt (
    workspace_id TEXT NOT NULL,
    pattern      TEXT NOT NULL,
    PRIMARY KEY (workspace_id, pattern)
) STRICT;
CREATE TABLE never_decrypt_global (
    pattern TEXT NOT NULL PRIMARY KEY
) STRICT;
";

const V11: &str = r"
-- How long Puddle leaves a host alone after a tool refused its certificate, and how many such
-- hosts a workspace remembers. One row for every workspace; no row means the built-in values.
CREATE TABLE learned_limits (
    id       INTEGER NOT NULL PRIMARY KEY CHECK (id = 1),
    ttl_secs INTEGER NOT NULL CHECK (ttl_secs > 0),
    cap      INTEGER NOT NULL CHECK (cap > 0)
) STRICT;
";

const V12: &str = r"
-- Rules on the method and path of a request (R-49): they only ever refuse. A global rule applies
-- to every workspace, a workspace rule to its own; rows of a workspace go with it. `methods` is
-- the upper-case names separated by commas (empty: every method); `path` is a canonical path
-- pattern.
CREATE TABLE request_rules (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    scope        TEXT    NOT NULL CHECK (scope IN ('global', 'workspace')),
    workspace_id TEXT,
    host         TEXT    NOT NULL,
    methods      TEXT    NOT NULL,
    path         TEXT    NOT NULL,
    created_at   INTEGER NOT NULL,
    created_by   TEXT    NOT NULL CHECK (created_by IN ('cli', 'ui', 'api')),
    CHECK ((scope = 'workspace') = (workspace_id IS NOT NULL))
) STRICT;
CREATE INDEX request_rules_workspace ON request_rules (workspace_id);
";

/// The colour of a rule set the user made: a JSON `Fill` (`{"kind":"solid","color":"#rrggbb"}`),
/// or `NULL` for the default of its number. Sets written before this column have `NULL`.
const V13: &str = r"
ALTER TABLE rule_sets ADD COLUMN fill TEXT;
";

const V14: &str = r"
-- Where in a request a secret's stand-in is swapped: NULL for header values, `path`, or
-- `query:<name>`. Only a secret has a place.
ALTER TABLE env_vars ADD COLUMN place TEXT CHECK (place IS NULL OR kind = 'secret');
";

/// The audit search box: one column with the text a person would type to find a record, lower
/// case (ASCII), one part per line: host (a rule's pattern), method, path and reason of a
/// connection, host and path of a rule on method and path. Existing rows are backfilled from
/// their lines; `AuditRecord::search_text` must give the same values (tested).
const V15: &str = r"
ALTER TABLE audit ADD COLUMN search TEXT;
UPDATE audit SET search = nullif(lower(
    CASE type
        WHEN 'connection' THEN
            ifnull(char(10) || nullif(json_extract(line, '$.host'), ''), '') ||
            ifnull(char(10) || nullif(json_extract(line, '$.method'), ''), '') ||
            ifnull(char(10) || nullif(json_extract(line, '$.path'), ''), '') ||
            ifnull(char(10) || nullif(json_extract(line, '$.reason'), ''), '')
        WHEN 'request_rule_created' THEN
            ifnull(char(10) || nullif(json_extract(line, '$.rule.host'), ''), '') ||
            ifnull(char(10) || nullif(json_extract(line, '$.rule.path'), ''), '')
        WHEN 'request_rule_deleted' THEN
            ifnull(char(10) || nullif(json_extract(line, '$.rule.host'), ''), '') ||
            ifnull(char(10) || nullif(json_extract(line, '$.rule.path'), ''), '')
        ELSE ifnull(char(10) || nullif(host, ''), '')
    END), '');
UPDATE audit SET search = substr(search, 2) WHERE search IS NOT NULL;
";

/// Brings `conn` to [`SCHEMA_VERSION`] and returns the version it started at.
///
/// # Errors
/// [`StoreError::SchemaTooNew`] for a database written by a newer Puddle (never downgraded),
/// or the SQLite error of a failed migration (which is rolled back).
pub(crate) fn migrate(conn: &mut Connection) -> Result<u32, StoreError> {
    migrate_with(conn, MIGRATIONS)
}

/// The name a workspace had when an audit record was written, so a record of a renamed or
/// deleted workspace still reads as the user knew it. `workspace_names` holds the current name
/// of each workspace (the host keeps it up to date); a new audit row copies it. Rows written
/// before this column have `NULL`, and a reader shows the id for them.
const V16: &str = r"
CREATE TABLE workspace_names (
    workspace_id TEXT PRIMARY KEY,
    name         TEXT NOT NULL
) STRICT;
ALTER TABLE audit ADD COLUMN workspace_name TEXT;
";

/// A rule may be made by a workspace create (`workspace_create`): the create allows the host of a
/// new workspace's repositories. SQLite cannot change a `CHECK`, so the table is rebuilt (as in
/// version 4) with the `AUTOINCREMENT` high-water mark carried over.
const V17: &str = r"
CREATE TABLE rules_v17 (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    scope             TEXT    NOT NULL CHECK (scope IN ('global', 'workspace', 'set')),
    workspace_id      TEXT,
    set_id            INTEGER REFERENCES rule_sets (id),
    pattern_kind      TEXT    NOT NULL CHECK (pattern_kind IN ('exact', 'suffix')),
    pattern           TEXT    NOT NULL,
    effect            TEXT    NOT NULL CHECK (effect IN ('allow', 'deny')),
    expires_at        INTEGER,
    created_at        INTEGER NOT NULL,
    created_by        TEXT    NOT NULL CHECK (created_by IN ('cli', 'ui', 'api', 'workspace_create')),
    source_pending_id INTEGER,
    CHECK ((scope = 'workspace') = (workspace_id IS NOT NULL)),
    CHECK ((scope = 'set') = (set_id IS NOT NULL))
) STRICT;
INSERT INTO rules_v17 (id, scope, workspace_id, set_id, pattern_kind, pattern, effect, expires_at,
                       created_at, created_by, source_pending_id)
    SELECT id, scope, workspace_id, set_id, pattern_kind, pattern, effect, expires_at,
           created_at, created_by, source_pending_id
    FROM rules;
DELETE FROM sqlite_sequence WHERE name = 'rules_v17';
INSERT INTO sqlite_sequence (name, seq) SELECT 'rules_v17', seq FROM sqlite_sequence WHERE name = 'rules';
DROP TABLE rules;
ALTER TABLE rules_v17 RENAME TO rules;
CREATE INDEX rules_workspace ON rules (workspace_id);
CREATE INDEX rules_expiry ON rules (expires_at) WHERE expires_at IS NOT NULL;
CREATE INDEX rules_set ON rules (set_id) WHERE set_id IS NOT NULL;
";

/// Three interception modes instead of one switch. A global row holds the user's mode
/// (`NULL` until they choose: the install then behaves as before) and the learning switch; a
/// workspace's own mode is `NULL` to follow it. Each workspace row of the old switch keeps what
/// it did: on is `all` (every host, learned hosts passed through), off is `known_only`.
const V18: &str = r"
CREATE TABLE interception_settings (
    id          INTEGER NOT NULL PRIMARY KEY CHECK (id = 1),
    mode        TEXT CHECK (mode IN ('all', 'all_except_curated', 'known_only')),
    learn       INTEGER NOT NULL CHECK (learn IN (0, 1)),
    notice_seen INTEGER NOT NULL CHECK (notice_seen IN (0, 1))
) STRICT;
CREATE TABLE workspace_interception_new (
    workspace_id TEXT NOT NULL PRIMARY KEY,
    mode         TEXT CHECK (mode IN ('all', 'all_except_curated', 'known_only'))
) STRICT;
INSERT INTO workspace_interception_new (workspace_id, mode)
    SELECT workspace_id, CASE decrypt_all WHEN 1 THEN 'all' ELSE 'known_only' END
    FROM workspace_interception;
DROP TABLE workspace_interception;
ALTER TABLE workspace_interception_new RENAME TO workspace_interception;
";

/// The hosts a workspace's tools refused Puddle's certificate for, kept past the in-memory pass-through
/// list: when a host was first and last refused and how many times, so a restart or an expired
/// pass-through does not forget a host that keeps refusing.
const V19: &str = r"
CREATE TABLE learned_hosts (
    workspace_id TEXT    NOT NULL,
    host         TEXT    NOT NULL,
    first_at     INTEGER NOT NULL,
    last_at      INTEGER NOT NULL,
    count        INTEGER NOT NULL CHECK (count >= 1),
    PRIMARY KEY (workspace_id, host)
) STRICT;
";

/// Users, their devices and who owns what (storage spec, users and ownership).
///
/// - `users`: one owner (the operator, user 1, made here and owning everything that exists) and
///   any number of plain users. No credential lives in this table or in `devices`.
/// - `account_passwords`, `account_invites`: the sign-in secrets (a password's hash, an invite's
///   digest), read and written only through `AccountSecrets`, never through `Store`.
/// - `workspace_owners`: the user of each workspace made by a user other than the owner. A
///   workspace with no row is the owner's; rows stay when the workspace is deleted (ids are never
///   reused), so its pending requests and audit records keep their user.
/// - `user_id` on the rows that are not tied to one workspace (global and set rules, rule sets,
///   their switches, rules on method and path, identities, environment variables, the Git
///   defaults, audit records), every existing row the owner's; names and the one default identity
///   become unique per user. Only `audit.user_id` is written from the workspace's user so far;
///   the others stay the owner's until writes become per user (reads take the user from the
///   workspace meanwhile). `identities` is rebuilt for that (its label was `UNIQUE` inline); the
///   workspaces' identity lists are kept aside meanwhile, because dropping the table would cascade
///   into them. `git_defaults` is rebuilt keyed by user.
const V20: &str = r"
CREATE TABLE users (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    role       TEXT    NOT NULL CHECK (role IN ('owner', 'user')),
    name       TEXT    NOT NULL COLLATE NOCASE UNIQUE CHECK (length(name) BETWEEN 1 AND 64),
    created_at INTEGER NOT NULL
) STRICT;
CREATE UNIQUE INDEX users_one_owner ON users (role) WHERE role = 'owner';
INSERT INTO users (id, role, name, created_at)
    VALUES (1, 'owner', 'owner', CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER));
CREATE TRIGGER users_owner_kept
BEFORE DELETE ON users
WHEN OLD.role = 'owner'
BEGIN
    SELECT RAISE(ABORT, 'the owner is never deleted');
END;
CREATE TRIGGER users_role_fixed
BEFORE UPDATE OF id, role ON users
WHEN NEW.id IS NOT OLD.id OR NEW.role IS NOT OLD.role
BEGIN
    SELECT RAISE(ABORT, 'a user''s id and role never change');
END;

CREATE TABLE devices (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id      INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    name         TEXT    NOT NULL CHECK (length(name) BETWEEN 1 AND 64),
    created_at   INTEGER NOT NULL,
    last_seen_at INTEGER NOT NULL,
    expires_at   INTEGER NOT NULL,
    revoked_at   INTEGER
) STRICT;
CREATE INDEX devices_user ON devices (user_id);

CREATE TABLE account_passwords (
    user_id    INTEGER PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    hash       TEXT    NOT NULL CHECK (hash <> ''),
    changed_at INTEGER NOT NULL
) STRICT;
CREATE TABLE account_invites (
    user_id    INTEGER PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    digest     BLOB    NOT NULL UNIQUE CHECK (length(digest) >= 16),
    expires_at INTEGER NOT NULL
) STRICT;

CREATE TABLE workspace_owners (
    workspace_id TEXT    NOT NULL PRIMARY KEY,
    user_id      INTEGER NOT NULL REFERENCES users (id)
) STRICT;
CREATE INDEX workspace_owners_user ON workspace_owners (user_id);
CREATE TRIGGER workspace_owner_fixed
BEFORE UPDATE ON workspace_owners
BEGIN
    SELECT RAISE(ABORT, 'a workspace''s user never changes');
END;

ALTER TABLE rules ADD COLUMN user_id INTEGER NOT NULL DEFAULT 1;
ALTER TABLE request_rules ADD COLUMN user_id INTEGER NOT NULL DEFAULT 1;
ALTER TABLE audit ADD COLUMN user_id INTEGER NOT NULL DEFAULT 1;

ALTER TABLE rule_sets ADD COLUMN user_id INTEGER NOT NULL DEFAULT 1;
DROP INDEX rule_sets_name;
CREATE UNIQUE INDEX rule_sets_name ON rule_sets (user_id, name COLLATE NOCASE);

ALTER TABLE rule_set_switches ADD COLUMN user_id INTEGER NOT NULL DEFAULT 1;
DROP INDEX rule_set_switches_key;
CREATE UNIQUE INDEX rule_set_switches_key
    ON rule_set_switches (user_id, rule_set, ifnull(workspace_id, ''));

ALTER TABLE env_vars ADD COLUMN user_id INTEGER NOT NULL DEFAULT 1;
DROP INDEX env_vars_key;
CREATE UNIQUE INDEX env_vars_key ON env_vars (user_id, ifnull(workspace_id, ''), name);

CREATE TEMP TABLE workspace_identities_kept AS SELECT * FROM workspace_identities;
CREATE TABLE identities_v20 (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id      INTEGER NOT NULL DEFAULT 1 REFERENCES users (id),
    label        TEXT    NOT NULL COLLATE NOCASE,
    author_name  TEXT    NOT NULL,
    author_email TEXT    NOT NULL,
    credentials  TEXT    NOT NULL,
    signing      TEXT    NOT NULL DEFAULT 'none' CHECK (signing IN ('none')),
    position     INTEGER NOT NULL,
    is_default   INTEGER NOT NULL DEFAULT 0 CHECK (is_default IN (0, 1)),
    created_at   INTEGER NOT NULL,
    changed_at   INTEGER NOT NULL,
    UNIQUE (user_id, label)
) STRICT;
INSERT INTO identities_v20 (id, user_id, label, author_name, author_email, credentials, signing,
                            position, is_default, created_at, changed_at)
    SELECT id, 1, label, author_name, author_email, credentials, signing, position, is_default,
           created_at, changed_at
    FROM identities;
DELETE FROM sqlite_sequence WHERE name = 'identities_v20';
INSERT INTO sqlite_sequence (name, seq)
    SELECT 'identities_v20', seq FROM sqlite_sequence WHERE name = 'identities';
DROP TABLE identities;
ALTER TABLE identities_v20 RENAME TO identities;
CREATE UNIQUE INDEX identities_one_default ON identities (user_id) WHERE is_default = 1;
DELETE FROM workspace_identities;
INSERT INTO workspace_identities SELECT * FROM workspace_identities_kept;
DROP TABLE workspace_identities_kept;

CREATE TABLE git_defaults_v20 (
    user_id          INTEGER PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    only_push_listed INTEGER NOT NULL CHECK (only_push_listed IN (0, 1)),
    only_pull_listed INTEGER NOT NULL CHECK (only_pull_listed IN (0, 1))
) STRICT;
INSERT INTO git_defaults_v20 SELECT 1, only_push_listed, only_pull_listed FROM git_defaults;
DROP TABLE git_defaults;
ALTER TABLE git_defaults_v20 RENAME TO git_defaults;
";

/// Browser sign-in.
///
/// - `account_sessions`: the digest of each device's refresh token (a SHA-256 of 32 random
///   bytes), written and read only through `AccountSecrets`. A device with no row can't refresh.
/// - `account_setup`: one row once the first visitor made the owner account, so resetting the
///   owner's password never reopens the first-visitor flow.
/// - `sign_in_methods`: which ways of signing in are switched on. Only passwords exist so far.
const V21: &str = r"
CREATE TABLE account_sessions (
    device_id      INTEGER PRIMARY KEY REFERENCES devices (id) ON DELETE CASCADE,
    refresh_digest BLOB    NOT NULL UNIQUE CHECK (length(refresh_digest) >= 16)
) STRICT;
CREATE TABLE account_setup (
    id           INTEGER PRIMARY KEY CHECK (id = 1),
    completed_at INTEGER NOT NULL
) STRICT;
CREATE TABLE sign_in_methods (
    method  TEXT    PRIMARY KEY CHECK (method IN ('password', 'passkey')),
    enabled INTEGER NOT NULL CHECK (enabled IN (0, 1))
) STRICT;
INSERT INTO sign_in_methods (method, enabled) VALUES ('password', 1);
";

/// Every workspace has a recorded user from now on; a workspace with none is nobody's.
///
/// - `workspace_owners_adoption`: one row until the first start of this build has recorded the
///   owner for the workspaces then listed (made before every create recorded its user), none
///   after. Taking the row and recording the owners is one transaction, so it happens once.
const V22: &str = r"
CREATE TABLE workspace_owners_adoption (
    id INTEGER PRIMARY KEY CHECK (id = 1)
) STRICT;
INSERT INTO workspace_owners_adoption (id) VALUES (1);
";

/// The hosts each workspace may use the user's SSH agent for. A row is the user's "on"; `keys`
/// holds the allowed keys' SHA-256 fingerprints one per line, `NULL` for every key.
const V23: &str = r"
CREATE TABLE ssh_agent_hosts (
    workspace_id TEXT    NOT NULL,
    host         TEXT    NOT NULL CHECK (length(host) BETWEEN 1 AND 253),
    keys         TEXT    CHECK (keys IS NULL OR length(keys) > 0),
    changed_at   INTEGER NOT NULL,
    PRIMARY KEY (workspace_id, host)
) STRICT;
";

/// Drops `DEFAULT 1` from the `user_id` columns V20 added. A default made an insert that forgot
/// the column write as the owner; now it fails (`NOT NULL`) and every write names its user. SQLite
/// cannot drop a default in place (rows written before `ADD COLUMN` read it from the schema), so
/// each table is rebuilt: new table, copy with every id, drop, rename, and the `AUTOINCREMENT`
/// high-water mark, indexes and triggers carried over. The runner runs this with foreign keys off.
const V24: &str = r"
CREATE TABLE rules_v24 (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    scope             TEXT    NOT NULL CHECK (scope IN ('global', 'workspace', 'set')),
    workspace_id      TEXT,
    set_id            INTEGER REFERENCES rule_sets (id),
    pattern_kind      TEXT    NOT NULL CHECK (pattern_kind IN ('exact', 'suffix')),
    pattern           TEXT    NOT NULL,
    effect            TEXT    NOT NULL CHECK (effect IN ('allow', 'deny')),
    expires_at        INTEGER,
    created_at        INTEGER NOT NULL,
    created_by        TEXT    NOT NULL CHECK (created_by IN ('cli', 'ui', 'api', 'workspace_create')),
    source_pending_id INTEGER,
    user_id           INTEGER NOT NULL,
    CHECK ((scope = 'workspace') = (workspace_id IS NOT NULL)),
    CHECK ((scope = 'set') = (set_id IS NOT NULL))
) STRICT;
INSERT INTO rules_v24 (id, scope, workspace_id, set_id, pattern_kind, pattern, effect, expires_at,
                       created_at, created_by, source_pending_id, user_id)
    SELECT id, scope, workspace_id, set_id, pattern_kind, pattern, effect, expires_at,
           created_at, created_by, source_pending_id, user_id
    FROM rules;
DELETE FROM sqlite_sequence WHERE name = 'rules_v24';
INSERT INTO sqlite_sequence (name, seq)
    SELECT 'rules_v24', seq FROM sqlite_sequence WHERE name = 'rules';
DROP TABLE rules;
ALTER TABLE rules_v24 RENAME TO rules;
CREATE INDEX rules_expiry ON rules (expires_at) WHERE expires_at IS NOT NULL;
CREATE INDEX rules_set ON rules (set_id) WHERE set_id IS NOT NULL;
CREATE INDEX rules_workspace ON rules (workspace_id);

CREATE TABLE request_rules_v24 (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    scope        TEXT    NOT NULL CHECK (scope IN ('global', 'workspace')),
    workspace_id TEXT,
    host         TEXT    NOT NULL,
    methods      TEXT    NOT NULL,
    path         TEXT    NOT NULL,
    created_at   INTEGER NOT NULL,
    created_by   TEXT    NOT NULL CHECK (created_by IN ('cli', 'ui', 'api')),
    user_id      INTEGER NOT NULL,
    CHECK ((scope = 'workspace') = (workspace_id IS NOT NULL))
) STRICT;
INSERT INTO request_rules_v24 (id, scope, workspace_id, host, methods, path, created_at,
                               created_by, user_id)
    SELECT id, scope, workspace_id, host, methods, path, created_at, created_by, user_id
    FROM request_rules;
DELETE FROM sqlite_sequence WHERE name = 'request_rules_v24';
INSERT INTO sqlite_sequence (name, seq)
    SELECT 'request_rules_v24', seq FROM sqlite_sequence WHERE name = 'request_rules';
DROP TABLE request_rules;
ALTER TABLE request_rules_v24 RENAME TO request_rules;
CREATE INDEX request_rules_workspace ON request_rules (workspace_id);

CREATE TABLE audit_v24 (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    ts           INTEGER NOT NULL,
    type         TEXT    NOT NULL,
    workspace_id TEXT,
    line         TEXT    NOT NULL CHECK (length(CAST(line AS BLOB)) <= 4096),
    host         TEXT,
    outcome      TEXT,
    search       TEXT,
    workspace_name TEXT,
    user_id      INTEGER NOT NULL
) STRICT;
INSERT INTO audit_v24 (id, ts, type, workspace_id, line, host, outcome, search, workspace_name,
                       user_id)
    SELECT id, ts, type, workspace_id, line, host, outcome, search, workspace_name, user_id
    FROM audit;
DELETE FROM sqlite_sequence WHERE name = 'audit_v24';
INSERT INTO sqlite_sequence (name, seq)
    SELECT 'audit_v24', seq FROM sqlite_sequence WHERE name = 'audit';
DROP TABLE audit;
ALTER TABLE audit_v24 RENAME TO audit;
CREATE INDEX audit_outcome ON audit (outcome, id) WHERE outcome IS NOT NULL;
CREATE INDEX audit_ts ON audit (ts);
CREATE INDEX audit_type ON audit (type, id);
CREATE INDEX audit_workspace ON audit (workspace_id, id);
CREATE TRIGGER audit_size_insert AFTER INSERT ON audit
BEGIN
    UPDATE audit_size SET bytes = bytes + length(CAST(NEW.line AS BLOB)) WHERE id = 1;
END;
CREATE TRIGGER audit_size_delete AFTER DELETE ON audit
BEGIN
    UPDATE audit_size SET bytes = bytes - length(CAST(OLD.line AS BLOB)) WHERE id = 1;
END;

CREATE TABLE rule_sets_v24 (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    name        TEXT    NOT NULL CHECK (length(name) BETWEEN 1 AND 64),
    description TEXT    NOT NULL DEFAULT '' CHECK (length(description) <= 500),
    created_at  INTEGER NOT NULL,
    created_by  TEXT    NOT NULL CHECK (created_by IN ('cli', 'ui', 'api')),
    fill        TEXT,
    user_id     INTEGER NOT NULL
) STRICT;
INSERT INTO rule_sets_v24 (id, name, description, created_at, created_by, fill, user_id)
    SELECT id, name, description, created_at, created_by, fill, user_id FROM rule_sets;
DELETE FROM sqlite_sequence WHERE name = 'rule_sets_v24';
INSERT INTO sqlite_sequence (name, seq)
    SELECT 'rule_sets_v24', seq FROM sqlite_sequence WHERE name = 'rule_sets';
DROP TABLE rule_sets;
ALTER TABLE rule_sets_v24 RENAME TO rule_sets;
CREATE UNIQUE INDEX rule_sets_name ON rule_sets (user_id, name COLLATE NOCASE);

CREATE TABLE rule_set_switches_v24 (
    rule_set     TEXT    NOT NULL,
    workspace_id TEXT,
    enabled      INTEGER NOT NULL CHECK (enabled IN (0, 1)),
    changed_at   INTEGER NOT NULL,
    user_id      INTEGER NOT NULL
) STRICT;
INSERT INTO rule_set_switches_v24 (rule_set, workspace_id, enabled, changed_at, user_id)
    SELECT rule_set, workspace_id, enabled, changed_at, user_id FROM rule_set_switches;
DROP TABLE rule_set_switches;
ALTER TABLE rule_set_switches_v24 RENAME TO rule_set_switches;
CREATE UNIQUE INDEX rule_set_switches_key
    ON rule_set_switches (user_id, rule_set, ifnull(workspace_id, ''));

CREATE TABLE env_vars_v24 (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    workspace_id TEXT,
    name         TEXT    NOT NULL,
    kind         TEXT    NOT NULL CHECK (kind IN ('plain', 'secret')),
    value        TEXT,
    secret_id    TEXT,
    hosts        TEXT,
    changed_at   INTEGER NOT NULL,
    place        TEXT CHECK (place IS NULL OR kind = 'secret'),
    user_id      INTEGER NOT NULL,
    CHECK ((kind = 'plain') = (value IS NOT NULL)),
    CHECK ((kind = 'secret') = (secret_id IS NOT NULL)),
    CHECK ((kind = 'secret') = (hosts IS NOT NULL))
) STRICT;
INSERT INTO env_vars_v24 (id, workspace_id, name, kind, value, secret_id, hosts, changed_at, place,
                          user_id)
    SELECT id, workspace_id, name, kind, value, secret_id, hosts, changed_at, place, user_id
    FROM env_vars;
DELETE FROM sqlite_sequence WHERE name = 'env_vars_v24';
INSERT INTO sqlite_sequence (name, seq)
    SELECT 'env_vars_v24', seq FROM sqlite_sequence WHERE name = 'env_vars';
DROP TABLE env_vars;
ALTER TABLE env_vars_v24 RENAME TO env_vars;
CREATE UNIQUE INDEX env_vars_key ON env_vars (user_id, ifnull(workspace_id, ''), name);

CREATE TABLE identities_v24 (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id      INTEGER NOT NULL REFERENCES users (id),
    label        TEXT    NOT NULL COLLATE NOCASE,
    author_name  TEXT    NOT NULL,
    author_email TEXT    NOT NULL,
    credentials  TEXT    NOT NULL,
    signing      TEXT    NOT NULL DEFAULT 'none' CHECK (signing IN ('none')),
    position     INTEGER NOT NULL,
    is_default   INTEGER NOT NULL DEFAULT 0 CHECK (is_default IN (0, 1)),
    created_at   INTEGER NOT NULL,
    changed_at   INTEGER NOT NULL,
    UNIQUE (user_id, label)
) STRICT;
INSERT INTO identities_v24 (id, user_id, label, author_name, author_email, credentials, signing,
                            position, is_default, created_at, changed_at)
    SELECT id, user_id, label, author_name, author_email, credentials, signing, position,
           is_default, created_at, changed_at
    FROM identities;
DELETE FROM sqlite_sequence WHERE name = 'identities_v24';
INSERT INTO sqlite_sequence (name, seq)
    SELECT 'identities_v24', seq FROM sqlite_sequence WHERE name = 'identities';
DROP TABLE identities;
ALTER TABLE identities_v24 RENAME TO identities;
CREATE UNIQUE INDEX identities_one_default ON identities (user_id) WHERE is_default = 1;
";

/// The built-in rule sets Puddle hides while the feature that needs them is off. Derived from the
/// settings at start and after every change (`SystemPlan::hidden_sets`); a hidden set decides
/// nothing and is left out of the list, and the switches stored for it are kept.
const V25: &str = r"
CREATE TABLE hidden_builtin_sets (
    slug TEXT PRIMARY KEY CHECK (length(slug) BETWEEN 1 AND 64)
) STRICT;
";

/// Migrates only up to `version`, to build the database an older Puddle left behind.
#[cfg(test)]
pub(crate) fn migrate_up_to(conn: &mut Connection, version: usize) -> Result<u32, StoreError> {
    migrate_with(conn, &MIGRATIONS[..version])
}

/// How many `puddle.db.pre-v<N>` copies stay next to the database.
const BACKUPS_KEPT: usize = 2;

/// One row of `PRAGMA foreign_key_check`: (child table, child rowid, parent table, constraint).
type Violation = (String, Option<i64>, String, i64);

fn migrate_with(conn: &mut Connection, migrations: &[&str]) -> Result<u32, StoreError> {
    let start: u32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let supported = u32::try_from(migrations.len()).unwrap_or(u32::MAX);
    if start > supported {
        return Err(StoreError::SchemaTooNew {
            found: start,
            supported,
        });
    }
    if start == supported {
        return Ok(start);
    }
    if start > 0 {
        back_up_before(conn, supported)?;
    }
    // The pragma is a no-op inside a transaction, so it is switched here. With it on, a table
    // rebuild (`DROP TABLE` of a parent) cascades deletes into the child tables.
    let foreign_keys: bool = conn.pragma_query_value(None, "foreign_keys", |row| row.get(0))?;
    conn.pragma_update(None, "foreign_keys", false)?;
    let result = run_steps(conn, migrations, start);
    if foreign_keys {
        conn.pragma_update(None, "foreign_keys", true)?;
    }
    result.map(|()| start)
}

fn run_steps(conn: &mut Connection, migrations: &[&str], start: u32) -> Result<(), StoreError> {
    // Violations a step did not cause (an older bug, a hand edit) never block the upgrade.
    let known = violations(conn)?;
    for (version, sql) in (start..).zip(migrations.iter().skip(start as usize)) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        if let Some((table, _, parent, _)) = violations(&tx)?
            .into_iter()
            .find(|found| !known.contains(found))
        {
            return Err(StoreError::MigrationBrokeForeignKeys {
                version: version + 1,
                table,
                parent,
            });
        }
        tx.pragma_update(None, "user_version", version + 1)?;
        tx.commit()?;
    }
    Ok(())
}

fn violations(conn: &Connection) -> Result<Vec<Violation>, StoreError> {
    let mut stmt = conn.prepare("PRAGMA foreign_key_check")?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Copies the database to `<file>.pre-v<target>` before the first step, so a bad release can be
/// rolled back by putting the copy in place. In-memory databases have nothing to copy.
fn back_up_before(conn: &Connection, target: u32) -> Result<(), StoreError> {
    let Some(path) = conn.path().filter(|p| !p.is_empty() && *p != ":memory:") else {
        return Ok(());
    };
    let path = Path::new(path);
    let backup = backup_path(path, target);
    let io = |err: std::io::Error| StoreError::Backup(format!("{}: {err}", backup.display()));
    // A failed earlier run may have left a copy; `VACUUM INTO` refuses an existing file.
    match std::fs::remove_file(&backup) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(io(err)),
    }
    conn.execute("VACUUM INTO ?1", [backup.to_string_lossy().as_ref()])?;
    prune_backups(path).map_err(|err| StoreError::Backup(err.to_string()))
}

fn backup_path(db: &Path, version: u32) -> PathBuf {
    let mut name = db.as_os_str().to_owned();
    name.push(format!(".pre-v{version}"));
    PathBuf::from(name)
}

/// Deletes all but the newest [`BACKUPS_KEPT`] copies next to `db`.
fn prune_backups(db: &Path) -> std::io::Result<()> {
    let dir = db
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let file = db.file_name().unwrap_or_default();
    let prefix = format!("{}.pre-v", file.to_string_lossy());
    let mut copies: Vec<(u32, PathBuf)> = std::fs::read_dir(dir)?
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let version = name.strip_prefix(&prefix)?.parse().ok()?;
            Some((version, entry.path()))
        })
        .collect();
    copies.sort_unstable_by_key(|copy| std::cmp::Reverse(copy.0));
    for (_, stale) in copies.into_iter().skip(BACKUPS_KEPT) {
        // Name the file that failed, not the copy just made.
        std::fs::remove_file(&stale).map_err(|err| {
            std::io::Error::new(err.kind(), format!("{}: {err}", stale.display()))
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
