// SPDX-License-Identifier: AGPL-3.0-or-later
//! The filtered audit queries behind `GET /api/audit`.
#![expect(
    clippy::unwrap_used,
    reason = "test helpers outside #[test] functions fail the test by panicking"
)]

use super::*;

// Audit queries

fn pending_wire(workspace: &str, host: &str, state: PendingState) -> PendingWire {
    PendingWire {
        id: 1,
        workspace_id: workspace.into(),
        host: host.into(),
        port: 443,
        first_seen: 1,
        last_seen: 1,
        attempts: 1,
        state,
        decided_at: None,
        decided_by: None,
        rule_id: None,
        rule_set: None,
        program: None,
    }
}

fn rule_wire(workspace: Option<&str>, pattern: &str) -> RuleWire {
    RuleWire {
        id: 1,
        scope: workspace.map_or(RuleWireScope::Global, |_| RuleWireScope::Workspace),
        workspace_id: workspace.map(Into::into),
        pattern_kind: RuleWirePatternKind::Exact,
        pattern: pattern.into(),
        effect: Effect::Allow,
        expires_at: None,
        created_at: 1,
        created_by: ActorWire::Cli,
        source_pending_id: None,
        set_id: None,
        precedence: 5,
    }
}

/// A connection record; an empty `workspace` is one of Puddle's own (no workspace).
fn connection(ts: u64, workspace: &str, host: &str, decision: ConnectionDecision) -> AuditRecord {
    AuditRecord::connection(ConnectionRecord {
        ts,
        workspace_id: (!workspace.is_empty()).then(|| workspace.to_owned()),
        origin: if workspace.is_empty() {
            ConnectionOrigin::Puddle
        } else {
            ConnectionOrigin::Workspace
        },
        host: Some(host.into()),
        port: Some(443),
        resolved_ip: None,
        upstream: None,
        decision: Some(decision),
        outcome: None,
        reason: "rule".into(),
        rule_id: None,
        rule_set: None,
        request_rule: None,
        refused_address: None,
        refused_reason: None,
        category: None,
        signal: None,
        pending_id: None,
        binding_id: None,
        injected: false,
        unswapped_stand_ins: Vec::new(),
        scrubbed_stand_ins: Vec::new(),
        foreign_credentials: Vec::new(),
        answer_credentials: Vec::new(),
        protocol: None,
        method: None,
        path: None,
        git_operation: None,
        path_truncated: false,
        bytes_up: 0,
        bytes_down: 0,
        count: None,
    })
}

/// `record` (a connection) as a request of `method` and `path`.
fn with_request(mut record: AuditRecord, method: &str, path: &str) -> AuditRecord {
    if let AuditRecord::Connection(c) = &mut record {
        c.method = Some(method.into());
        c.path = Some(path.into());
    }
    record
}

/// A store on disk, with `records` inserted the way `Store` inserts them (same columns).
fn seeded(records: &[AuditRecord]) -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("puddle.db");
    let clock = Arc::new(ManualClock::new(T0));
    let store = Store::open(&path, clock, Limits::default()).unwrap();
    own_workspaces(&store);
    reseed(&seed_connection(&path), records);
    (dir, store)
}

/// The connection that writes the audit rows beside the store's own. Nothing here must survive a
/// crash, so it does not wait for the disk: with the default `synchronous` every seeding commit
/// is an fsync, and on a busy host those waits, not the work, made the test slow.
fn seed_connection(path: &Path) -> rusqlite::Connection {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.busy_timeout(Duration::from_secs(5)).unwrap();
    conn.pragma_update(None, "synchronous", "off").unwrap();
    conn
}

/// Replaces the audit rows with `records`, numbered from 1 again, so one opened store serves many
/// generated cases (opening runs every migration, which a 48-case property would otherwise pay 48
/// times).
fn reseed(conn: &rusqlite::Connection, records: &[AuditRecord]) {
    let tx = conn.unchecked_transaction().unwrap();
    tx.execute_batch("DELETE FROM audit; DELETE FROM sqlite_sequence WHERE name = 'audit';")
        .unwrap();
    {
        let mut insert = tx
            .prepare(
                "INSERT INTO audit (ts, type, workspace_id, host, outcome, line, search, category,
                                    user_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 1)",
            )
            .unwrap();
        for record in records {
            insert
                .execute(params![
                    i64::try_from(record.ts()).unwrap(),
                    record.kind(),
                    record.workspace_id(),
                    record.host(),
                    record.outcome().map(AuditOutcome::as_str),
                    record.to_line().unwrap(),
                    record.search_text(),
                    record.category(),
                ])
                .unwrap();
        }
    }
    tx.commit().unwrap();
}

fn naive(records: &[AuditRecord], filter: &AuditFilter) -> Vec<i64> {
    records
        .iter()
        .zip(1_i64..)
        .filter(|(r, _)| {
            filter
                .workspace
                .as_ref()
                .is_none_or(|s| r.workspace_id() == Some(s.as_str()))
                && filter.kind.is_none_or(|k| r.kind() == k)
                && filter.outcome.is_none_or(|o| r.outcome() == Some(o))
                && filter.origin.is_none_or(|o| r.origin() == Some(o))
                && filter.from.is_none_or(|t| r.ts() >= t)
                && filter.to.is_none_or(|t| r.ts() < t)
                && filter.host_contains.as_ref().is_none_or(|h| {
                    r.host()
                        .is_some_and(|host| host.contains(&h.to_ascii_lowercase()))
                })
                && filter.q.as_ref().is_none_or(|q| {
                    r.search_text()
                        .is_some_and(|text| text.contains(&q.to_ascii_lowercase()))
                })
        })
        .map(|(_, id)| id)
        .collect()
}

fn ids(rows: Vec<(i64, String)>) -> Vec<i64> {
    rows.into_iter().map(|(id, _)| id).collect()
}

fn any_record() -> impl Strategy<Value = AuditRecord> {
    let workspace = prop::sample::select(vec!["a", "b", "c"]);
    let host = prop::sample::select(vec![
        "example.com",
        "api.example.com",
        "github.com",
        "Xn--bcher-kva.example",
        "10.0.0.1",
    ]);
    let ts = (0_u64..20).prop_map(|t| T0 + t * 1000);
    let decision = prop::sample::select(vec![
        ConnectionDecision::Allow,
        ConnectionDecision::Deny,
        ConnectionDecision::Pending,
        ConnectionDecision::Blocked,
    ]);
    let state = prop::sample::select(vec![
        PendingState::Requested,
        PendingState::Allowed,
        PendingState::Denied,
        PendingState::Expired,
    ]);
    (0..8_u8, workspace, host, ts, decision, state).prop_map(|(kind, sb_, host, ts, d, st)| {
        let host = host.to_lowercase();
        match kind {
            0 => with_request(connection(ts, sb_, &host, d), "GET", "/V1/Items"),
            1 => connection(ts, "", &host, d),
            2 => AuditRecord::PendingCreated {
                ts,
                pending: pending_wire(sb_, &host, PendingState::Requested),
            },
            3 => AuditRecord::PendingDecided {
                ts,
                pending: pending_wire(sb_, &host, st),
            },
            4 => AuditRecord::PendingExpired {
                ts,
                pending: pending_wire(sb_, &host, PendingState::Expired),
                reason: PendingExpiryReason::Stale,
            },
            5 => AuditRecord::RuleCreated {
                ts,
                rule: rule_wire(Some(sb_), &host),
            },
            6 => AuditRecord::RuleDeleted {
                ts,
                rule: rule_wire(None, &format!(".{host}")),
                reason: RuleDeleteReason::User,
                actor: "cli".into(),
            },
            _ => AuditRecord::PendingSuppressed {
                ts,
                workspace_id: sb_.into(),
                count: 3,
            },
        }
    })
}

fn any_filter() -> impl Strategy<Value = AuditFilter> {
    (
        prop::option::of(prop::sample::select(vec!["a", "b", "zzz"])),
        prop::option::of(prop::sample::select(AuditRecord::KINDS.to_vec())),
        prop::option::of(prop::sample::select(AuditOutcome::ALL.to_vec())),
        prop::option::of(prop::sample::select(vec![
            ConnectionOrigin::Workspace,
            ConnectionOrigin::Puddle,
        ])),
        prop::option::of(prop::sample::select(vec![
            "example",
            "EXAMPLE.com",
            ".example",
            "10.0",
            "nothing",
            "git",
        ])),
        prop::option::of(prop::sample::select(vec![
            "EXAMPLE", "get", "/v1", "rule", "nothing",
        ])),
        prop::option::of(0_u64..22),
        prop::option::of(0_u64..22),
    )
        .prop_map(
            |(workspace, kind, outcome, origin, host, q, from, to)| AuditFilter {
                user: None,
                workspace: workspace.map(sb),
                kind,
                outcome,
                origin,
                host_contains: host.map(str::to_owned),
                q: q.map(str::to_owned),
                category: None,
                from: from.map(|t| T0 + t * 1000),
                to: to.map(|t| T0 + t * 1000),
            },
        )
}

/// Every filter combination returns exactly what filtering the records by hand returns,
/// in both directions, and paging back with `Before` visits each match once.
///
/// One store serves all cases (each case replaces the audit rows): opening a store runs every
/// migration, which made 48 opens the bulk of the test's time.
#[test]
fn filtered_queries_equal_a_naive_filter() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("puddle.db");
    let store = Store::open(&path, Arc::new(ManualClock::new(T0)), Limits::default()).unwrap();
    own_workspaces(&store);
    let seed = seed_connection(&path);
    let users = puddle_store::testing::all_users();

    let cases = (
        prop::collection::vec(any_record(), 0..60),
        any_filter(),
        1_u32..9,
    );
    let mut runner = proptest::test_runner::TestRunner::new(ProptestConfig::with_cases(48));
    runner
        .run(&cases, |(records, filter, page)| {
            reseed(&seed, &records);
            let want = naive(&records, &filter);
            let up = ids(store
                .audit_query_every_user(&users, &filter, AuditCursor::After(0), 1000)
                .unwrap());
            prop_assert_eq!(&up, &want);
            let mut down = ids(store
                .audit_query_every_user(&users, &filter, AuditCursor::Before(None), 1000)
                .unwrap());
            down.reverse();
            prop_assert_eq!(&down, &want);

            let mut paged = Vec::new();
            let mut cursor = AuditCursor::Before(None);
            loop {
                let rows = ids(store
                    .audit_query_every_user(&users, &filter, cursor, page)
                    .unwrap());
                let Some(last) = rows.last().copied() else {
                    break;
                };
                prop_assert!(rows.len() <= page as usize);
                paged.extend(rows);
                cursor = AuditCursor::Before(Some(last));
            }
            paged.reverse();
            prop_assert_eq!(paged, want);
            Ok(())
        })
        .unwrap();
}

#[test]
fn filters_combine_and_the_host_is_case_folded() {
    let records = vec![
        connection(T0, "a", "api.example.com", ConnectionDecision::Allow),
        connection(T0 + 1, "a", "api.example.com", ConnectionDecision::Deny),
        connection(T0 + 2, "b", "api.example.com", ConnectionDecision::Allow),
        AuditRecord::RuleCreated {
            ts: T0 + 3,
            rule: rule_wire(Some("a"), "api.example.com"),
        },
    ];
    let (_dir, store) = seeded(&records);
    let q = |filter: AuditFilter| {
        ids(store
            .audit_query_every_user(
                &puddle_store::testing::all_users(),
                &filter,
                AuditCursor::After(0),
                100,
            )
            .unwrap())
    };
    assert_eq!(q(AuditFilter::default()), [1, 2, 3, 4]);
    assert_eq!(
        q(AuditFilter {
            workspace: Some(sb("a")),
            kind: Some("connection"),
            outcome: Some(AuditOutcome::Allow),
            host_contains: Some("API.EXAMPLE".into()),
            ..AuditFilter::default()
        }),
        [1]
    );
    // A rule matches on its pattern, and has no outcome.
    assert_eq!(
        q(AuditFilter {
            host_contains: Some("example".into()),
            kind: Some("rule_created"),
            ..AuditFilter::default()
        }),
        [4]
    );
    assert_eq!(
        q(AuditFilter {
            outcome: Some(AuditOutcome::Expired),
            ..AuditFilter::default()
        }),
        Vec::<i64>::new()
    );
    // `from` is inclusive and `to` exclusive.
    assert_eq!(
        q(AuditFilter {
            from: Some(T0 + 1),
            to: Some(T0 + 3),
            ..AuditFilter::default()
        }),
        [2, 3]
    );
}

#[test]
fn the_host_needle_folds_ascii_only_like_the_column_it_matches() {
    let (_dir, store) = seeded(&[connection(
        T0,
        "a",
        "kite.example.com",
        ConnectionDecision::Allow,
    )]);
    let q = |needle: &str| {
        ids(store
            .audit_query_every_user(
                &puddle_store::testing::all_users(),
                &AuditFilter {
                    host_contains: Some(needle.into()),
                    ..AuditFilter::default()
                },
                AuditCursor::After(0),
                100,
            )
            .unwrap())
    };
    assert_eq!(q("KITE"), [1]);
    // The Kelvin sign lower-cases to an ASCII `k` under Unicode folding, and no host has it.
    assert_eq!(q("\u{212A}ite"), Vec::<i64>::new());
}

#[test]
fn the_category_filter_matches_company_network_connections_only() {
    let mut company = connection(T0, "a", "nexus.corp.example", ConnectionDecision::Allow);
    let mut refused = connection(T0 + 1, "a", "10.8.0.9", ConnectionDecision::Deny);
    let mut lookalike = connection(
        T0 + 2,
        "a",
        "company_network.example",
        ConnectionDecision::Allow,
    );
    for (record, reason) in [
        (&mut company, "rule"),
        (&mut refused, "toggle:company_network"),
    ] {
        if let AuditRecord::Connection(c) = record {
            c.reason = reason.into();
            c.category = Some("company_network".into());
        }
    }
    if let AuditRecord::Connection(c) = &mut lookalike {
        c.reason = "toggle:company_network".into();
    }
    let records = vec![
        company,
        refused,
        lookalike,
        connection(T0 + 3, "a", "api.example.com", ConnectionDecision::Allow),
        // A guest chooses its request method: one named like the category is not in it.
        with_request(
            connection(T0 + 4, "a", "api.example.com", ConnectionDecision::Allow),
            "COMPANY_NETWORK",
            "/",
        ),
    ];
    let (_dir, store) = seeded(&records);
    let found = |category: Option<&'static str>| {
        ids(store
            .audit_query_every_user(
                &puddle_store::testing::all_users(),
                &AuditFilter {
                    category,
                    ..AuditFilter::default()
                },
                AuditCursor::After(0),
                100,
            )
            .unwrap())
    };
    assert_eq!(found(Some("company_network")), [1, 2]);
    assert_eq!(found(None), [1, 2, 3, 4, 5]);
}

#[test]
fn the_search_box_matches_each_text_column_and_combines_with_the_other_filters() {
    let mut denied = connection(T0 + 1, "b", "evil.example.org", ConnectionDecision::Deny);
    if let AuditRecord::Connection(c) = &mut denied {
        c.reason = "request_rule".into();
    }
    let records = vec![
        // 1: host, method and path.
        with_request(
            connection(T0, "a", "api.example.com", ConnectionDecision::Allow),
            "POST",
            "/Orders/42",
        ),
        // 2: the reason.
        denied,
        // 3: a rule's pattern.
        AuditRecord::RuleCreated {
            ts: T0 + 2,
            rule: rule_wire(Some("a"), ".Corp.Internal"),
        },
        // 4: a pending request's host.
        AuditRecord::PendingCreated {
            ts: T0 + 3,
            pending: pending_wire("a", "queued.example.net", PendingState::Requested),
        },
        // 5: a rule on method and path, by its host and path.
        AuditRecord::RequestRuleCreated {
            ts: T0 + 4,
            rule: RequestRuleWire {
                id: 1,
                scope: RequestRuleWireScope::Global,
                workspace_id: None,
                host: "files.example.io".into(),
                methods: vec!["DELETE".into()],
                path: "/Admin/**".into(),
                created_at: T0,
                created_by: ActorWire::Ui,
                precedence: 5,
            },
        },
        // 6: no searchable text at all.
        AuditRecord::PendingSuppressed {
            ts: T0 + 5,
            workspace_id: "a".into(),
            count: 2,
        },
    ];
    let (_dir, store) = seeded(&records);
    let found = |filter: AuditFilter| {
        ids(store
            .audit_query_every_user(
                &puddle_store::testing::all_users(),
                &filter,
                AuditCursor::After(0),
                100,
            )
            .unwrap())
    };
    let q = |text: &str| {
        found(AuditFilter {
            q: Some(text.into()),
            ..AuditFilter::default()
        })
    };
    assert_eq!(q("api.example"), [1], "host");
    assert_eq!(q("POST"), [1], "method, case-folded");
    assert_eq!(q("orders/42"), [1], "path, case-folded");
    assert_eq!(q("request_rule"), [2], "reason");
    assert_eq!(q("corp.internal"), [3], "rule pattern");
    assert_eq!(q("queued"), [4], "pending host");
    assert_eq!(q("files.example.io"), [5], "method and path rule host");
    assert_eq!(q("/admin/"), [5], "method and path rule path");
    assert_eq!(q("EXAMPLE"), [1, 2, 4, 5], "several columns, several kinds");
    assert_eq!(q("nothing-like-it"), Vec::<i64>::new());
    // Text that spans two fields is no hit (the API refuses a line break in `q`, the fields'
    // separator).
    assert_eq!(q("com post"), Vec::<i64>::new());
    // Every other filter narrows the search, and `q` narrows them.
    let with = |q: &str, filter: AuditFilter| {
        found(AuditFilter {
            q: Some(q.into()),
            ..filter
        })
    };
    assert_eq!(
        with(
            "example",
            AuditFilter {
                workspace: Some(sb("a")),
                ..AuditFilter::default()
            }
        ),
        [1, 4]
    );
    assert_eq!(
        with(
            "example",
            AuditFilter {
                kind: Some("connection"),
                ..AuditFilter::default()
            }
        ),
        [1, 2]
    );
    assert_eq!(
        with(
            "example",
            AuditFilter {
                outcome: Some(AuditOutcome::Deny),
                ..AuditFilter::default()
            }
        ),
        [2]
    );
    assert_eq!(
        with(
            "example",
            AuditFilter {
                origin: Some(ConnectionOrigin::Workspace),
                ..AuditFilter::default()
            }
        ),
        [1, 2]
    );
    assert_eq!(
        with(
            "example",
            AuditFilter {
                from: Some(T0 + 1),
                to: Some(T0 + 4),
                ..AuditFilter::default()
            }
        ),
        [2, 4]
    );
    assert_eq!(
        with(
            "example",
            AuditFilter {
                host_contains: Some("evil".into()),
                ..AuditFilter::default()
            }
        ),
        [2]
    );
    assert_eq!(
        with(
            "orders",
            AuditFilter {
                workspace: Some(sb("b")),
                ..AuditFilter::default()
            }
        ),
        Vec::<i64>::new()
    );
}

#[test]
fn hostile_filter_text_is_data_not_sql_or_a_pattern() {
    let (_dir, store) = seeded(&[connection(
        T0,
        "a",
        "example.com",
        ConnectionDecision::Allow,
    )]);
    for needle in ["%", "_", "' OR 1=1 --", "\"; DROP TABLE audit; --", "\u{0}"] {
        let rows = store
            .audit_query_every_user(
                &puddle_store::testing::all_users(),
                &AuditFilter {
                    host_contains: Some(needle.into()),
                    ..AuditFilter::default()
                },
                AuditCursor::After(0),
                10,
            )
            .unwrap();
        assert!(rows.is_empty(), "{needle:?}");
        let rows = store
            .audit_query_every_user(
                &puddle_store::testing::all_users(),
                &AuditFilter {
                    q: Some(needle.into()),
                    ..AuditFilter::default()
                },
                AuditCursor::After(0),
                10,
            )
            .unwrap();
        assert!(rows.is_empty(), "q {needle:?}");
    }
    assert_eq!(
        store
            .audit_query_every_user(
                &puddle_store::testing::all_users(),
                &AuditFilter::default(),
                AuditCursor::After(0),
                10
            )
            .unwrap()
            .len(),
        1
    );
}

#[test]
#[expect(clippy::too_many_lines, reason = "one table of query cases")]
fn a_hundred_thousand_records_answer_every_filter_quickly() {
    const ROWS: u64 = 100_000;
    // A full scan takes 4 steps a row (400 014 steps measured); a rescan per row takes ROWS times that.
    const STEP_BAR: u64 = 8 * ROWS;
    let workspaces = ["alpha", "beta", "gamma", "delta"];
    let decisions = [
        ConnectionDecision::Allow,
        ConnectionDecision::Deny,
        ConnectionDecision::Pending,
        ConnectionDecision::Blocked,
    ];
    let records: Vec<AuditRecord> = (0..ROWS)
        .map(|i| {
            let workspace = workspaces[(i % 4) as usize];
            let ts = T0 + i * 10;
            match i % 50 {
                0 => AuditRecord::RuleCreated {
                    ts,
                    rule: rule_wire(Some(workspace), &format!("rule{i}.example.com")),
                },
                1 => AuditRecord::PendingCreated {
                    ts,
                    pending: pending_wire(
                        workspace,
                        &format!("host{i}.example.org"),
                        PendingState::Requested,
                    ),
                },
                _ => with_request(
                    connection(
                        ts,
                        workspace,
                        &format!("h{}.site{}.example.net", i % 97, i % 13),
                        decisions[(i % 4) as usize],
                    ),
                    "GET",
                    &format!("/v1/items/{i}/details"),
                ),
            }
        })
        .collect();
    let (_dir, store) = seeded(&records);
    let last = T0 + ROWS * 10;
    // Each case names the plan step it must use. The wall clock below is only a backstop: the
    // plan is what proves a filter doesn't fall back to scanning the whole table, and it
    // doesn't depend on how busy the machine is.
    let cases: Vec<(&str, AuditFilter, AuditCursor, &str)> = vec![
        (
            "newest page",
            AuditFilter::default(),
            AuditCursor::Before(None),
            "SCAN audit",
        ),
        (
            "older page",
            AuditFilter::default(),
            AuditCursor::Before(Some(50_000)),
            "USING INTEGER PRIMARY KEY (rowid<?)",
        ),
        (
            "workspace",
            AuditFilter {
                workspace: Some(sb("beta")),
                ..AuditFilter::default()
            },
            AuditCursor::Before(None),
            "USING INDEX audit_workspace (workspace_id=?)",
        ),
        (
            "type",
            AuditFilter {
                kind: Some("pending_created"),
                ..AuditFilter::default()
            },
            AuditCursor::Before(None),
            "USING INDEX audit_type (type=?)",
        ),
        (
            "outcome",
            AuditFilter {
                outcome: Some(AuditOutcome::Blocked),
                ..AuditFilter::default()
            },
            AuditCursor::Before(None),
            // Several filters have an index to search by, and with no statistics SQLite's pick
            // among them follows the width of the table, which a column added to it moved. Any
            // of them bounds the work; the plan must not be a scan.
            "SEARCH audit USING INDEX audit_",
        ),
        (
            "last hour (all of it)",
            AuditFilter {
                from: Some(last - 3_600_000),
                ..AuditFilter::default()
            },
            AuditCursor::Before(None),
            "SCAN audit",
        ),
        (
            "narrow time range",
            AuditFilter {
                from: Some(T0 + 400_000),
                to: Some(T0 + 410_000),
                ..AuditFilter::default()
            },
            AuditCursor::Before(None),
            "USING INDEX audit_ts (ts>? AND ts<?)",
        ),
        (
            "host, common",
            AuditFilter {
                host_contains: Some("EXAMPLE.net".into()),
                ..AuditFilter::default()
            },
            AuditCursor::Before(None),
            "SCAN audit",
        ),
        (
            "host, rare (scans everything)",
            AuditFilter {
                host_contains: Some("rule99950".into()),
                ..AuditFilter::default()
            },
            AuditCursor::Before(None),
            "SCAN audit",
        ),
        (
            "host, absent (scans everything)",
            AuditFilter {
                host_contains: Some("absent.invalid".into()),
                ..AuditFilter::default()
            },
            AuditCursor::Before(None),
            "SCAN audit",
        ),
        (
            "search, common (host, method and path text)",
            AuditFilter {
                q: Some("EXAMPLE.net".into()),
                ..AuditFilter::default()
            },
            AuditCursor::Before(None),
            "SCAN audit",
        ),
        (
            "search, common word in the path",
            AuditFilter {
                q: Some("details".into()),
                ..AuditFilter::default()
            },
            AuditCursor::Before(None),
            "SCAN audit",
        ),
        (
            "search, rare (a path near the oldest record: scans everything)",
            AuditFilter {
                q: Some("/v1/items/52/details".into()),
                ..AuditFilter::default()
            },
            AuditCursor::Before(None),
            "SCAN audit",
        ),
        (
            "search, absent (scans everything)",
            AuditFilter {
                q: Some("absent.invalid".into()),
                ..AuditFilter::default()
            },
            AuditCursor::Before(None),
            "SCAN audit",
        ),
        (
            "search with a workspace and a type",
            AuditFilter {
                q: Some("absent.invalid".into()),
                workspace: Some(sb("beta")),
                kind: Some("connection"),
                ..AuditFilter::default()
            },
            AuditCursor::Before(None),
            "USING INDEX audit_workspace (workspace_id=?)",
        ),
        (
            "everything at once",
            AuditFilter {
                user: Some(UserId::OWNER),
                workspace: Some(sb("gamma")),
                kind: Some("connection"),
                outcome: Some(AuditOutcome::Pending),
                origin: Some(ConnectionOrigin::Workspace),
                q: Some("details".into()),
                host_contains: Some("site7".into()),
                category: None,
                from: Some(T0),
                to: Some(last),
            },
            AuditCursor::Before(None),
            // Several filters have an index to search by, and with no statistics SQLite's pick
            // among them follows the width of the table, which a column added to it moved. Any
            // of them bounds the work; the plan must not be a scan.
            "SEARCH audit USING INDEX audit_",
        ),
        (
            "tail",
            AuditFilter::default(),
            AuditCursor::After(99_900),
            "USING INTEGER PRIMARY KEY (rowid>?)",
        ),
    ];
    for (name, filter, cursor, step) in cases {
        let plan = store.audit_query_plan(&filter, cursor, 100).unwrap();
        assert!(
            plan.iter().any(|line| line.contains(step)),
            "{name}: expected `{step}` in the plan, got {plan:?}"
        );
        let all = puddle_store::testing::all_users();
        let rows = store
            .audit_query_every_user(&all, &filter, cursor, 100)
            .unwrap();
        assert!(rows.len() <= 100);
        // The bar is on the work the query does, counted in SQLite's virtual-machine steps, which no
        // other process on the machine changes. It catches a query that went quadratic (a per-row
        // rescan multiplies the steps by the table size). The slowest case here is a full scan.
        let steps = store.audit_query_steps(&all, &filter, cursor, 100).unwrap();
        eprintln!("audit query {name}: {steps} steps");
        let steps = u64::try_from(steps).unwrap();
        // Every row returned took at least one step, so a count that does not cover them is no count.
        assert!(
            steps > rows.len() as u64,
            "{name} returned {} rows in {steps} steps",
            rows.len()
        );
        assert!(
            steps < STEP_BAR,
            "{name} took {steps} steps over {ROWS} rows"
        );
    }
    // The filtered results are right, not just fast.
    let one = store
        .audit_query_every_user(
            &puddle_store::testing::all_users(),
            &AuditFilter {
                host_contains: Some("rule99950".into()),
                ..AuditFilter::default()
            },
            AuditCursor::Before(None),
            100,
        )
        .unwrap();
    assert_eq!(one.len(), 1);
}
#[test]
fn a_users_audit_query_returns_only_that_users_records_when_the_filter_names_no_user() {
    let (_, _, store) = fixture(Limits::default());
    let ann = store.add_user(&UserName::new("Ann").unwrap()).unwrap().id;
    store.set_workspace_owner(&sb("anns"), ann).unwrap();
    ask(&store, "a", "owner.example");
    ask(&store, "anns", "ann.example");

    let page = |user: UserId| -> Vec<String> {
        store
            .audit_query(user, &AuditFilter::default(), AuditCursor::After(0), 100)
            .unwrap()
            .into_iter()
            .map(|(_, line)| line)
            .collect()
    };
    let owners = page(UserId::OWNER);
    let anns = page(ann);
    assert_eq!(owners.len(), 1, "{owners:?}");
    assert!(owners[0].contains("owner.example"), "{owners:?}");
    assert_eq!(anns.len(), 1, "{anns:?}");
    assert!(anns[0].contains("ann.example"), "{anns:?}");
}
