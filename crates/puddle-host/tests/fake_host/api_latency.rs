// SPDX-License-Identifier: AGPL-3.0-or-later
//! The read calls the UI polls, on a database the size of a long-lived install: 100 000 audit
//! records, 1000 rules, 1000 open requests. Each call's cost is the CPU time the process spent on
//! it, which a busy machine does not stretch the way it stretches a wall clock. Meaningful only in
//! a release build, so they are ignored by default; the nightly `perf` workflow runs them.

use super::*;

use puddle_store::{Actor, Effect, NewRule, Pattern, Scope};

const AUDIT_RECORDS: u32 = 100_000;
const RULES: u32 = 1000;
const PENDING: u32 = 1000;
const CALLS: usize = 200;
/// The bar of the soak playbook: the p95 of one call's CPU time.
const P95_BAR: Duration = Duration::from_millis(100);

/// CPU time this process has used, summed over its threads' scheduler statistics (`schedstat`
/// counts nanoseconds on a CPU). A thread that ended in between takes its time with it, which only
/// lowers a reading.
fn cpu_time() -> Duration {
    let tasks = std::fs::read_dir("/proc/self/task").unwrap();
    let nanos: u64 = tasks
        .filter_map(|task| {
            let stat = std::fs::read_to_string(task.ok()?.path().join("schedstat")).ok()?;
            stat.split_whitespace().next()?.parse::<u64>().ok()
        })
        .sum();
    Duration::from_nanos(nanos)
}

/// A host with workspace `acme`, `RULES` rules, `PENDING` open requests and `AUDIT_RECORDS` audit
/// records.
async fn host_with_a_long_history(rig: &Rig) -> Host<FakeRuntime> {
    host_with_audit_records(rig, AUDIT_RECORDS, 1).await
}

/// [`host_with_a_long_history`] with `records` audit records, each `step_ms` older than the next.
async fn host_with_audit_records(rig: &Rig, records: u32, step_ms: u32) -> Host<FakeRuntime> {
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let store = host.store().as_user(puddle_store::UserId::OWNER);
    for n in 0..RULES {
        let rule = NewRule {
            scope: Scope::Global,
            pattern: Pattern::parse(&format!("rule{n}.allowed.example")).unwrap(),
            effect: Effect::Allow,
            expires_at: None,
            created_by: Actor::Cli,
        };
        store.add_rule(&rule).unwrap();
    }
    for n in 0..PENDING {
        let request = puddle_types::EgressRequest::new(
            name("acme"),
            puddle_types::Host::parse_normalised(&format!("ask{n}.unknown.example")).unwrap(),
            443,
        );
        host.store()
            .decide(&request, puddle_types::SuffixAllows::Count)
            .unwrap();
    }
    // The records the store wrote above are the template: the rest are copies, dated before them.
    let db = rusqlite::Connection::open(rig.dir.path().join("data").join("puddle.db")).unwrap();
    db.busy_timeout(Duration::from_secs(30)).unwrap();
    db.execute(
        "INSERT INTO audit (ts, type, workspace_id, line, host, outcome, search, workspace_name, user_id)
         WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ?1)
         SELECT t.ts - n.i * ?2, t.type, t.workspace_id, t.line, t.host, t.outcome, t.search,
                t.workspace_name, t.user_id
         FROM n, (SELECT * FROM audit WHERE type = 'connection' OR host IS NOT NULL ORDER BY id LIMIT 1) t",
        [records, step_ms],
    )
    .unwrap();
    let kept: u32 = db
        .query_row("SELECT count(*) FROM audit", [], |r| r.get(0))
        .unwrap();
    assert!(kept >= records, "{kept} audit records");
    drop(db);
    host
}

/// The p95 of `CALLS` calls' CPU time.
async fn p95_of(api: &Api, path: &str) -> Duration {
    p95_of_calls(api, path, CALLS).await
}

/// The p95 of `calls` calls' CPU time.
#[expect(
    clippy::print_stderr,
    reason = "the nightly log shows what was measured, not only the verdict"
)]
async fn p95_of_calls(api: &Api, path: &str, calls: usize) -> Duration {
    let mut spent = Vec::with_capacity(calls);
    for _ in 0..calls {
        let before = cpu_time();
        let reply = api.get(path).await;
        let after = cpu_time();
        assert_eq!(reply.status, 200, "{path}: {}", reply.body);
        spent.push(after.saturating_sub(before));
    }
    spent.sort();
    let p95 = spent[calls * 95 / 100];
    eprintln!(
        "{path}: p95 {p95:?}, median {:?}, worst {:?}",
        spent[calls / 2],
        spent[calls - 1]
    );
    p95
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "a large database, meaningful only in a release build; run by the nightly perf workflow or by hand"]
async fn the_polled_api_calls_stay_under_100_ms_of_cpu_on_a_long_history() {
    let rig = Rig::named_ids();
    let host = host_with_a_long_history(&rig).await;
    let api = api(&host);
    let counted = api.get("/api/pending").await.json();
    assert_eq!(
        counted["requests"].as_array().unwrap().len(),
        PENDING as usize
    );
    let mut over = Vec::new();
    for path in [
        "/api/health",
        "/api/workspaces",
        "/api/pending",
        "/api/audit?limit=200",
    ] {
        let p95 = p95_of(&api, path).await;
        if p95 >= P95_BAR {
            over.push(format!("{path}: p95 {p95:?}"));
        }
    }
    assert!(over.is_empty(), "over {P95_BAR:?} of CPU: {over:?}");
    host.shutdown().await;
}

/// Audit records at the volume the cap allows in about seven hours: 5 million.
const AUDIT_RECORDS_AT_VOLUME: u32 = 5_000_000;

/// What the Activity screen asks for (page of 200, each filter, the next older page, the tail,
/// a search that matches nothing) on 5 million records spread over a day. Prints the p95 of
/// each call's CPU time; fails on none, because the decision is the reader's.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "5 million records (several GiB), meaningful only in a release build; run by hand"]
async fn the_activity_screen_calls_on_five_million_audit_records() {
    let rig = Rig::named_ids();
    let built = std::time::Instant::now();
    // 17 ms apart: five million records span a day, so a time range cuts.
    let host = host_with_audit_records(&rig, AUDIT_RECORDS_AT_VOLUME, 17).await;
    let db = rig.dir.path().join("data").join("puddle.db");
    eprintln!(
        "built in {:?}; database {} MiB, audit_bytes {} MiB",
        built.elapsed(),
        std::fs::metadata(&db).unwrap().len() >> 20,
        host.store().audit_bytes().unwrap() >> 20
    );
    let api = api(&host);
    let (newest, newest_ts): (i64, i64) = {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.query_row("SELECT max(id), max(ts) FROM audit", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap()
    };
    let from = newest_ts - 3_600_000;
    let deep = newest - i64::from(AUDIT_RECORDS_AT_VOLUME) / 2;
    let paths = [
        "/api/audit?limit=200".to_owned(),
        "/api/audit?limit=200&workspace=acme".to_owned(),
        "/api/audit?limit=200&type=connection".to_owned(),
        "/api/audit?limit=200&outcome=allow".to_owned(),
        "/api/audit?limit=200&category=company_network".to_owned(),
        format!("/api/audit?limit=200&from={from}"),
        format!("/api/audit?limit=200&before={deep}"),
        format!("/api/audit?limit=500&after={}", newest - 1000),
        "/api/audit?limit=200&host_contains=example".to_owned(),
        "/api/audit?limit=200&q=example".to_owned(),
        "/api/audit?limit=200&q=nosuchword".to_owned(),
        "/api/audit?limit=200&host_contains=nosuchword".to_owned(),
        "/api/audit?limit=200&workspace=acme&type=connection&outcome=deny&q=nosuchword".to_owned(),
        "/api/audit/retention".to_owned(),
        "/api/health".to_owned(),
    ];
    for path in &paths {
        // Fewer calls than the 100k test: a full scan of 5 million records takes seconds.
        p95_of_calls(&api, path, 20).await;
    }
    host.shutdown().await;
}
