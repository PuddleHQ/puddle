-- Rows for the golden database `golden-v23.db`: one Puddle install at schema version 23 with
-- every table filled, nullable columns both set and null, and ids that skip a deleted row.
-- `schema::tests::write_golden_database` builds the file from the migrations up to version 23
-- plus this file; never regenerate it with a newer schema, the point is an old database.
UPDATE users SET name = 'tijs', created_at = 1700000000 WHERE id = 1;
INSERT INTO users (id, role, name, created_at) VALUES
    (2, 'user', 'Ann', 1700000100),
    (3, 'user', 'bob', 1700000200);
INSERT OR REPLACE INTO workspace_owners_adoption (id) VALUES (1);
INSERT INTO devices (id, user_id, name, created_at, last_seen_at, expires_at, revoked_at) VALUES
    (1, 2, 'ann desktop', 1700000300, 1700000400, 1800000000, NULL),
    (2, 3, 'bob phone', 1700000310, 1700000410, 1800000010, 1700000500);
INSERT INTO account_passwords (user_id, hash, changed_at) VALUES (2, '$argon2id$v=19$golden-hash-ann', 1700000320);
INSERT INTO account_invites (user_id, digest, expires_at) VALUES (3, x'00112233445566778899aabbccddeeff00', 1700009999);
INSERT INTO account_sessions (device_id, refresh_digest) VALUES (1, x'ffeeddccbbaa99887766554433221100ff');
INSERT OR REPLACE INTO account_setup (id, completed_at) VALUES (1, 1700000050);
INSERT OR REPLACE INTO sign_in_methods (method, enabled) VALUES ('password', 1), ('passkey', 0);

INSERT INTO rule_sets (id, name, description, created_at, created_by, fill, user_id) VALUES
    (1, 'Python', 'pip and pypi', 1700001000, 'ui', 'blue', 1),
    (2, 'Mine', '', 1700001010, 'api', NULL, 2);
INSERT INTO rules (id, scope, workspace_id, set_id, pattern_kind, pattern, effect, expires_at, created_at,
                   created_by, source_pending_id, user_id) VALUES
    (1, 'global', NULL, NULL, 'exact', 'api.example.com', 'allow', NULL, 1700002000, 'ui', NULL, 1),
    (2, 'global', NULL, NULL, 'suffix', '.blocked-site.com', 'deny', NULL, 1700002010, 'cli', NULL, 1),
    (3, 'workspace', 'ws-alpha', NULL, 'suffix', '.github.com', 'allow', 1900000000, 1700002020, 'api', 2, 1),
    (4, 'set', NULL, 1, 'exact', 'pypi.org', 'allow', NULL, 1700002030, 'ui', NULL, 1),
    (5, 'workspace', 'ws-beta', NULL, 'exact', 'created.example', 'allow', NULL, 1700002040, 'workspace_create', NULL, 2),
    (7, 'set', NULL, 2, 'suffix', '.ann-site.com', 'deny', NULL, 1700002050, 'ui', NULL, 2);
INSERT INTO request_rules (id, scope, workspace_id, host, methods, path, created_at, created_by, user_id) VALUES
    (1, 'global', NULL, 'api.example.com', 'GET,HEAD', '/v1/*', 1700003000, 'ui', 1),
    (3, 'workspace', 'ws-alpha', 'github.com', '*', '/PuddleHQ/*', 1700003010, 'api', 2);
INSERT INTO rule_set_switches (rule_set, workspace_id, enabled, changed_at, user_id) VALUES
    ('1', NULL, 1, 1700004000, 1),
    ('1', 'ws-alpha', 0, 1700004010, 1),
    ('2', NULL, 1, 1700004020, 2);
INSERT INTO system_reasons (workspace_id, reason) VALUES (NULL, 'no-proxy-set'), ('ws-alpha', 'host-unreachable');
INSERT INTO builtin_sets_seen (slug, entries, changed_at) VALUES
    ('retired-python', '["pypi.org","files.pythonhosted.org"]', 1700005000),
    ('retired-rust', '["crates.io"]', NULL);

INSERT INTO pending (id, workspace_id, host, port, first_seen, last_seen, attempts, state, decided_at, decided_by,
                     rule_id, rule_set) VALUES
    (1, 'ws-alpha', 'api.example.com', 443, 1700006000, 1700006100, 3, 'allowed', 1700006200, 'ui', 1, NULL),
    (2, 'ws-alpha', 'github.com', 443, 1700006010, 1700006110, 2, 'allowed', 1700006210, 'cli', 3, 'Python'),
    (3, 'ws-beta', 'evil.example', 80, 1700006020, 1700006120, 7, 'denied', 1700006220, 'api', 2, NULL),
    (4, 'ws-beta', 'old.example', 8080, 1700006030, 1700006130, 1, 'expired', 1700006230, 'system', NULL, NULL),
    (5, 'ws-alpha', 'waiting.example', 443, 1700006040, 1700006140, 4, 'requested', NULL, NULL, NULL, NULL);

INSERT INTO audit (id, ts, type, workspace_id, line, host, outcome, search, workspace_name, user_id) VALUES
    (1, 1700007000, 'request_allowed', 'ws-alpha', '{"type":"request_allowed","host":"api.example.com"}', 'api.example.com', 'allowed', 'api.example.com', 'Alpha', 1),
    (2, 1700007010, 'rule_created', NULL, '{"type":"rule_created","id":1}', NULL, NULL, NULL, NULL, 1),
    (4, 1700007020, 'request_denied', 'ws-beta', '{"type":"request_denied","host":"evil.example"}', 'evil.example', 'denied', 'evil.example', 'Beta', 2);
INSERT INTO audit_size (id, bytes) VALUES (1, 0)
    ON CONFLICT (id) DO UPDATE SET bytes = (SELECT sum(length(CAST(line AS BLOB))) FROM audit);

INSERT INTO workspace_names (workspace_id, name) VALUES ('ws-alpha', 'Alpha'), ('ws-beta', 'Beta');
INSERT INTO workspace_owners (workspace_id, user_id) VALUES ('ws-alpha', 1), ('ws-beta', 2);
INSERT INTO workspace_repos (id, workspace_id, host, owner, repo, pull, push, created_at) VALUES
    (1, 'ws-alpha', 'github.com', 'PuddleHQ', 'puddle', 1, 0, 1700008000),
    (2, 'ws-alpha', 'github.com', 'sharkdp', 'hyperfine', 1, 1, 1700008010);
INSERT INTO workspace_git (workspace_id, only_push_listed, only_pull_listed) VALUES
    ('ws-alpha', 1, NULL), ('ws-beta', NULL, 0);
INSERT OR REPLACE INTO git_defaults (user_id, only_push_listed, only_pull_listed) VALUES (1, 1, 0), (2, 0, 1);
INSERT INTO identities (id, user_id, label, author_name, author_email, credentials, signing, position, is_default,
                        created_at, changed_at) VALUES
    (1, 1, 'Work', 'Tijs Van Kampen', 'tijs@work.example', '[{"host":"github.com","source":{"kind":"gh","host":"github.com","account":"tijs"},"covers":{"owners":["PuddleHQ"],"rest_of_host":true}}]', 'none', 0, 1, 1700009000, 1700009100),
    (2, 1, 'home', 'Tijs', 'tijs@home.example', '[]', 'none', 1, 0, 1700009010, 1700009110),
    (3, 2, 'ann', 'Ann', 'ann@x.example', '[{"host":"gitlab.com","source":{"kind":"gh","host":"gitlab.com","account":"ann"},"covers":{"owners":[],"rest_of_host":true}}]', 'none', 0, 1, 1700009020, 1700009120);
INSERT INTO workspace_identities (workspace_id, identity_id, position) VALUES
    ('ws-alpha', 1, 0), ('ws-alpha', 2, 1), ('ws-beta', 3, 0);

INSERT INTO env_vars (id, workspace_id, name, kind, value, secret_id, hosts, changed_at, place, user_id) VALUES
    (1, NULL, 'EDITOR', 'plain', 'vim', NULL, NULL, 1700010000, NULL, 1),
    (2, 'ws-alpha', 'GH_TOKEN', 'secret', NULL, 'secret-gh', '["github.com"]', 1700010010, 'path', 1),
    (3, 'ws-beta', 'LANG', 'plain', 'en_BE.UTF-8', NULL, NULL, 1700010020, NULL, 2);
INSERT INTO env_stand_ins (workspace_id, env_id, stand_in) VALUES ('ws-alpha', 2, 'puddle-standin-0123456789abcdef');
INSERT INTO shell_init (workspace_id, content, changed_at) VALUES ('ws-alpha', 'export PS1="> "', 1700011000);
INSERT INTO pinned_ports (workspace_id, guest_port, host_port) VALUES ('ws-alpha', 3000, 13000), ('ws-alpha', 5432, 15432);
INSERT INTO ssh_agent_hosts (workspace_id, host, keys, changed_at) VALUES
    ('ws-alpha', 'github.com', '["SHA256:abc"]', 1700012000), ('ws-beta', 'git.example', NULL, 1700012010);

INSERT OR REPLACE INTO interception_settings (id, mode, learn, notice_seen) VALUES (1, 'known_only', 1, 1);
INSERT INTO workspace_interception (workspace_id, mode) VALUES ('ws-alpha', 'all'), ('ws-beta', NULL);
INSERT INTO never_decrypt (workspace_id, pattern) VALUES ('ws-alpha', '*.bank.example');
INSERT INTO never_decrypt_global (pattern) VALUES ('secret.example');
INSERT OR REPLACE INTO learned_limits (id, ttl_secs, cap) VALUES (1, 86400, 500);
INSERT INTO learned_hosts (workspace_id, host, first_at, last_at, count) VALUES
    ('ws-alpha', 'cdn.example', 1700013000, 1700013500, 12);

-- Deleted rows leave gaps in the AUTOINCREMENT counters (the migration must keep the high-water marks).
UPDATE sqlite_sequence SET seq = 9 WHERE name = 'rules';
