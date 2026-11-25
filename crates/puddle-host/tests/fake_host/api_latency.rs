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
         SELECT t.ts - n.i, t.type, t.workspace_id, t.line, t.host, t.outcome, t.search,
                t.workspace_name, t.user_id
         FROM n, (SELECT * FROM audit WHERE type = 'connection' OR host IS NOT NULL ORDER BY id LIMIT 1) t",
        [AUDIT_RECORDS],
    )
    .unwrap();
    let kept: u32 = db
        .query_row("SELECT count(*) FROM audit", [], |r| r.get(0))
        .unwrap();
    assert!(kept >= AUDIT_RECORDS, "{kept} audit records");
    drop(db);
    host
}

/// The p95 of `CALLS` calls' CPU time.
#[expect(
    clippy::print_stderr,
    reason = "the nightly log shows what was measured, not only the verdict"
)]
async fn p95_of(api: &Api, path: &str) -> Duration {
    let mut spent = Vec::with_capacity(CALLS);
    for _ in 0..CALLS {
        let before = cpu_time();
        let reply = api.get(path).await;
        let after = cpu_time();
        assert_eq!(reply.status, 200, "{path}: {}", reply.body);
        spent.push(after.saturating_sub(before));
    }
    spent.sort();
    let p95 = spent[CALLS * 95 / 100];
    eprintln!(
        "{path}: p95 {p95:?}, median {:?}, worst {:?}",
        spent[CALLS / 2],
        spent[CALLS - 1]
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
