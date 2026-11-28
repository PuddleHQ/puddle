// SPDX-License-Identifier: AGPL-3.0-or-later
//! Versioned schema migrations, tracked in SQLite's `user_version`.
//!
//! Migrations only ever get appended. The steps run in one write transaction together with the
//! version bump, so a crash leaves the database at the version it had, never in between, and a
//! second process opening the same file (the host and a CLI command at once) waits, then finds
//! it migrated.
//!
//! The SQL of each step lives here; `migrations` runs them.

mod first_steps;
mod migrations;

use first_steps::{V1, V2, V3, V4, V5, V6, V7, V8, V9, V10, V11, V12, V13, V14, V15};

#[cfg(test)]
pub(crate) use migrations::migrate_up_to;
pub(crate) use migrations::{
    known_trigger_names, migrate, migrate_without_copy, remove_stale_partial_copies,
};

/// Schema migrations; entry `n` takes the database from version `n` to `n + 1`.
const MIGRATIONS: &[&str] = &[
    V1, V2, V3, V4, V5, V6, V7, V8, V9, V10, V11, V12, V13, V14, V15, V16, V17, V18, V19, V20, V21,
    V22, V23, V24, V25, V26, V27, V28, V29, V30, V31, V32, V33, V34, V35, V36,
];

/// The schema version this build writes.
pub const SCHEMA_VERSION: u32 = 36;

/// The name a workspace had when an audit record was written, so a record of a renamed or
/// deleted workspace still reads as the user knew it. `workspace_names` holds the current name
/// of each workspace (the host keeps it up to date); a new audit row copies it. Rows written
/// before this column have `NULL`, and a reader shows the id for them.
const V16: &str = "
CREATE TABLE workspace_names (
    workspace_id TEXT PRIMARY KEY,
    name         TEXT NOT NULL
) STRICT;
ALTER TABLE audit ADD COLUMN workspace_name TEXT;
";

/// A rule may be made by a workspace create (`workspace_create`): the create allows the host of a
/// new workspace's repositories. SQLite cannot change a `CHECK`, so the table is rebuilt (as in
/// version 4) with the `AUTOINCREMENT` high-water mark carried over.
const V17: &str = "
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
const V18: &str = "
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
const V19: &str = "
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
const V20: &str = "
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
const V21: &str = "
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
const V22: &str = "
CREATE TABLE workspace_owners_adoption (
    id INTEGER PRIMARY KEY CHECK (id = 1)
) STRICT;
INSERT INTO workspace_owners_adoption (id) VALUES (1);
";

/// The hosts each workspace may use the user's SSH agent for. A row is the user's "on"; `keys`
/// holds the allowed keys' SHA-256 fingerprints one per line, `NULL` for every key.
const V23: &str = "
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
const V24: &str = "
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
const V25: &str = "
CREATE TABLE hidden_builtin_sets (
    slug TEXT PRIMARY KEY CHECK (length(slug) BETWEEN 1 AND 64)
) STRICT;
";

/// The admin screen: a user the owner is deleting stays listed as `deleting` until their
/// workspaces are gone, and every user-management action is written to an audit that outlives the
/// user it names. The audit has no foreign key and holds names, never a credential.
const V26: &str = "
ALTER TABLE users ADD COLUMN deleting_at INTEGER;
CREATE TABLE admin_audit (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    at             INTEGER NOT NULL,
    actor_user_id  INTEGER NOT NULL,
    actor_name     TEXT    NOT NULL,
    action         TEXT    NOT NULL CHECK (action IN
        ('user_created', 'invite_reissued', 'user_delete_started', 'user_deleted')),
    target_user_id INTEGER NOT NULL,
    target_name    TEXT    NOT NULL
) STRICT;
";

/// The filtered SSH agent's first-use question. An `ssh_agent_asks` row is one question for a
/// workspace, host and key (one open at a time, `requested`) and what the user answered. An
/// `ssh_agent_grants` row is a standing yes for that triple: `run` ones last until the workspace
/// stops, the others until the user turns the agent off for the host.
const V27: &str = "
CREATE TABLE ssh_agent_asks (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    workspace_id TEXT    NOT NULL,
    host         TEXT    NOT NULL CHECK (length(host) BETWEEN 1 AND 253),
    fingerprint  TEXT    NOT NULL CHECK (length(fingerprint) BETWEEN 1 AND 100),
    key_type     TEXT    NOT NULL CHECK (length(key_type) BETWEEN 1 AND 40),
    state        TEXT    NOT NULL CHECK (state IN
                     ('requested', 'allowed_once', 'allowed_run', 'allowed_always', 'denied')),
    first_seen   INTEGER NOT NULL,
    last_seen    INTEGER NOT NULL,
    attempts     INTEGER NOT NULL CHECK (attempts >= 1),
    decided_at   INTEGER,
    CHECK ((state = 'requested') = (decided_at IS NULL))
) STRICT;
CREATE UNIQUE INDEX ssh_agent_asks_open
    ON ssh_agent_asks (workspace_id, host, fingerprint) WHERE state = 'requested';
CREATE INDEX ssh_agent_asks_decided ON ssh_agent_asks (decided_at) WHERE decided_at IS NOT NULL;
CREATE TABLE ssh_agent_grants (
    workspace_id TEXT    NOT NULL,
    host         TEXT    NOT NULL,
    fingerprint  TEXT    NOT NULL,
    run          INTEGER NOT NULL CHECK (run IN (0, 1)),
    created_at   INTEGER NOT NULL,
    PRIMARY KEY (workspace_id, host, fingerprint)
) STRICT;
";

/// How many days the audit is kept. One row, absent until the owner chooses (the store then uses
/// its default). The records older than this are deleted by the sweeper; there is no cap per
/// workspace, because a workspace that floods its own log could then push out its history.
const V28: &str = "
CREATE TABLE audit_retention (
    id   INTEGER NOT NULL PRIMARY KEY CHECK (id = 1),
    days INTEGER NOT NULL CHECK (days BETWEEN 1 AND 3650)
) STRICT;
";

const V29: &str = "
-- A rule can be switched off without losing it (R-65). Added columns: every row that exists is on.
ALTER TABLE rules ADD COLUMN enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1));
ALTER TABLE request_rules ADD COLUMN enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1));

-- One row per built-in entry the user switched off, kept apart from the catalogue (R-36, R-65).
-- `rule_set` is `builtin:<slug>`, `pattern` the entry as the catalogue writes it. An entry with no
-- row is on; switching it on again deletes the row, so the table holds only what differs.
CREATE TABLE builtin_entry_switches (
    user_id    INTEGER NOT NULL,
    rule_set   TEXT    NOT NULL,
    pattern    TEXT    NOT NULL,
    changed_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, rule_set, pattern)
) STRICT;
";

const V30: &str = "
-- Every rule has a precedence from 1 to 9, higher wins (R-59). Added columns: a rule stored
-- before precedence existed reads as 5, and no answer is kept (nothing is migrated).
ALTER TABLE rules ADD COLUMN precedence INTEGER NOT NULL DEFAULT 5
    CHECK (precedence BETWEEN 1 AND 9);
ALTER TABLE request_rules ADD COLUMN precedence INTEGER NOT NULL DEFAULT 5
    CHECK (precedence BETWEEN 1 AND 9);
-- A group's default for a new rule in it; none means 5 (R-59).
ALTER TABLE rule_sets ADD COLUMN default_precedence INTEGER
    CHECK (default_precedence IS NULL OR default_precedence BETWEEN 1 AND 9);
";

/// The filtered SSH agent's host key per workspace and host: the SHA-256 fingerprint of the first
/// host key a session bind held with, `NULL` until then. It goes with the row, so turning the
/// agent off and on again for the host accepts the next key.
const V31: &str = "
ALTER TABLE ssh_agent_hosts ADD COLUMN host_key TEXT
    CHECK (host_key IS NULL OR length(host_key) BETWEEN 1 AND 100);
";

const V32: &str = "
-- A waiting request whose name resolved to a local address needs an exact allow (R-14). The row
-- says so, so that closing waiting rows after a rule or set change asks for the same kind of
-- allow the proxy asked for. A row stored before this column reads as not needing one.
ALTER TABLE pending ADD COLUMN exact_only INTEGER NOT NULL DEFAULT 0 CHECK (exact_only IN (0, 1));
";

const V33: &str = "
-- The name of the program in the guest that opened the connection, as the agent reported it
-- when the row was opened. A display hint: the first program seen for the row stays. A row from
-- before this column, or one whose client the agent could not name, reads NULL.
ALTER TABLE pending ADD COLUMN program TEXT
    CHECK (program IS NULL OR length(program) BETWEEN 1 AND 32);
";

const V34: &str = "
-- Why a push to a repository cannot succeed, as its listing said when the row was made
-- ('archived' on its host, or 'read' for an account that can only read it). A row made before
-- this column, or from a listing that named no block, reads NULL.
ALTER TABLE workspace_repos ADD COLUMN push_block TEXT
    CHECK (push_block IS NULL OR push_block IN ('archived', 'read'));
";
/// The destination category of a connection record gets a column of its own, so the category
/// filter compares a column instead of matching a line of the search text (a guest chooses its
/// request method, which is a line there). Existing rows are backfilled from their lines. The
/// audit size now counts the `search` and `category` text as well as the line, which a guest's
/// path could fill without being counted; the total is recomputed.
const V35: &str = "
ALTER TABLE audit ADD COLUMN category TEXT;
UPDATE audit SET category = nullif(json_extract(line, '$.category'), '') WHERE type = 'connection';
DROP TRIGGER audit_size_insert;
DROP TRIGGER audit_size_delete;
CREATE TRIGGER audit_size_insert AFTER INSERT ON audit
BEGIN
    UPDATE audit_size SET bytes = bytes + length(CAST(NEW.line AS BLOB))
        + coalesce(length(CAST(NEW.search AS BLOB)), 0)
        + coalesce(length(CAST(NEW.category AS BLOB)), 0) WHERE id = 1;
END;
CREATE TRIGGER audit_size_delete AFTER DELETE ON audit
BEGIN
    UPDATE audit_size SET bytes = bytes - length(CAST(OLD.line AS BLOB))
        - coalesce(length(CAST(OLD.search AS BLOB)), 0)
        - coalesce(length(CAST(OLD.category AS BLOB)), 0) WHERE id = 1;
END;
UPDATE audit_size SET bytes = (
    SELECT coalesce(sum(length(CAST(line AS BLOB)) + coalesce(length(CAST(search AS BLOB)), 0)
                        + coalesce(length(CAST(category AS BLOB)), 0)), 0) FROM audit
) WHERE id = 1;
";

/// The activity screen and the admin stats read the audit at millions of rows, so they read
/// maintained figures instead of scanning it. `audit_size.records` counts the rows, and
/// `audit_minutes` holds the rows and line bytes written in each minute of their `ts` (what the
/// last-day and last-week figures sum); the insert and delete triggers keep both. The category
/// filter gets an index to walk, like the other equality filters have.
const V36: &str = "
ALTER TABLE audit_size ADD COLUMN records INTEGER NOT NULL DEFAULT 0;
UPDATE audit_size SET records = (SELECT count(*) FROM audit) WHERE id = 1;
CREATE TABLE audit_minutes (
    minute  INTEGER PRIMARY KEY,
    records INTEGER NOT NULL,
    bytes   INTEGER NOT NULL
) STRICT;
INSERT INTO audit_minutes (minute, records, bytes)
    SELECT ts / 60000, count(*), sum(length(CAST(line AS BLOB))) FROM audit GROUP BY ts / 60000;
CREATE INDEX audit_category ON audit (category, id) WHERE category IS NOT NULL;
DROP TRIGGER audit_size_insert;
DROP TRIGGER audit_size_delete;
CREATE TRIGGER audit_size_insert AFTER INSERT ON audit
BEGIN
    UPDATE audit_size SET records = records + 1,
        bytes = bytes + length(CAST(NEW.line AS BLOB))
        + coalesce(length(CAST(NEW.search AS BLOB)), 0)
        + coalesce(length(CAST(NEW.category AS BLOB)), 0) WHERE id = 1;
    INSERT INTO audit_minutes (minute, records, bytes)
        VALUES (NEW.ts / 60000, 1, length(CAST(NEW.line AS BLOB)))
        ON CONFLICT (minute) DO UPDATE SET records = records + 1, bytes = bytes + excluded.bytes;
END;
CREATE TRIGGER audit_size_delete AFTER DELETE ON audit
BEGIN
    UPDATE audit_size SET records = records - 1,
        bytes = bytes - length(CAST(OLD.line AS BLOB))
        - coalesce(length(CAST(OLD.search AS BLOB)), 0)
        - coalesce(length(CAST(OLD.category AS BLOB)), 0) WHERE id = 1;
    UPDATE audit_minutes SET records = records - 1,
        bytes = bytes - length(CAST(OLD.line AS BLOB)) WHERE minute = OLD.ts / 60000;
END;
";

#[cfg(test)]
mod tests;
