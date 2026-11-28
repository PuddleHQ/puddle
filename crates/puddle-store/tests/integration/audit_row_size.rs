// SPDX-License-Identifier: AGPL-3.0-or-later
//! How many bytes one connection record costs on disk, by shape, and so how fast the database grows
//! at the per-workspace cap (`Limits::connection_records_per_second`, 200 a second). Not a gate:
//! it prints the numbers the audit-size decision rests on, so it is ignored by default.
//! Run by hand: `cargo test -p puddle-store --test integration audit_row_size -- --ignored --nocapture`.
#![expect(
    clippy::unwrap_used,
    clippy::print_stderr,
    reason = "a measurement that prints its numbers and fails by panicking"
)]

use std::sync::Arc;

use puddle_store::testing::own;
use puddle_store::{Limits, ManualClock, Store};
use puddle_types::{
    ConnectionDecision, ConnectionEvent, ConnectionLog, ConnectionOutcome, ConnectionReason,
    EgressRequest, Host, HttpRequestLine, WorkspaceId,
};
use rusqlite::Connection;

const RECORDS: u32 = 50_000;
const CAP_PER_SECOND: u64 = 200;

fn workspace() -> WorkspaceId {
    WorkspaceId::new("acme").unwrap()
}

/// Event number `n` of a shape: a tunnel to an allowed host (the proxy saw no request), an
/// intercepted HTTPS request with a path, or a refusal.
fn event(shape: &str, n: u32) -> ConnectionEvent {
    let host = Host::parse_normalised(&format!("service{n}.region{}.example.com", n % 17)).unwrap();
    let request = EgressRequest::new(workspace(), host, 443);
    match shape {
        "tunnel" => {
            let mut e =
                ConnectionEvent::new(&request, ConnectionDecision::Allow, ConnectionReason::Rule);
            e.resolved_ip = Some(format!("93.184.{}.{}", n % 250, n % 200).parse().unwrap());
            e.outcome = Some(ConnectionOutcome::Connected);
            e.rule_id = Some(puddle_types::RuleId(1 + i64::from(n % 40)));
            e.bytes_up = 4_000 + u64::from(n % 5000);
            e.bytes_down = 90_000 + u64::from(n);
            e
        }
        "http" => {
            let mut e =
                ConnectionEvent::new(&request, ConnectionDecision::Allow, ConnectionReason::Rule);
            e.resolved_ip = Some(format!("140.82.{}.{}", n % 250, n % 200).parse().unwrap());
            e.outcome = Some(ConnectionOutcome::Connected);
            e.rule_id = Some(puddle_types::RuleId(1 + i64::from(n % 40)));
            e.http = Some(HttpRequestLine::new(
                "GET",
                &format!(
                    "/v2/organisations/acme/repositories/project-{n}/releases/assets/{n}.tar.gz"
                ),
            ));
            e.bytes_up = 800;
            e.bytes_down = 250_000 + u64::from(n);
            e
        }
        _ => {
            let mut e =
                ConnectionEvent::new(&request, ConnectionDecision::Deny, ConnectionReason::Rule);
            e.rule_id = Some(puddle_types::RuleId(1 + i64::from(n % 40)));
            e
        }
    }
}

fn file_bytes(db: &std::path::Path) -> u64 {
    let conn = Connection::open(db).unwrap();
    conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
        .unwrap();
    let pages: i64 = conn
        .pragma_query_value(None, "page_count", |r| r.get(0))
        .unwrap();
    let size: i64 = conn
        .pragma_query_value(None, "page_size", |r| r.get(0))
        .unwrap();
    u64::try_from(pages * size).unwrap()
}

#[test]
#[ignore = "a measurement; prints the bytes per record, run by hand"]
fn bytes_per_connection_record_by_shape_and_database_growth_a_day_at_the_cap() {
    let mut shapes = Vec::new();
    for shape in ["tunnel", "http", "denied"] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("puddle.db");
        let clock = Arc::new(ManualClock::new(1_800_000_000_000));
        let store = Store::open(
            &path,
            clock.clone(),
            Limits {
                connection_records_per_second: u32::MAX,
                ..Limits::default()
            },
        )
        .unwrap();
        own(&store, &workspace());
        let empty = file_bytes(&path);
        for n in 0..RECORDS {
            clock.advance(5);
            store.record(&event(shape, n));
        }
        let grown = file_bytes(&path) - empty;
        let lines = store.audit_bytes().unwrap();
        let rows = u64::from(RECORDS);
        let on_disk = grown / rows;
        let counted = lines / rows;
        let per_day = on_disk * CAP_PER_SECOND * 86_400;
        eprintln!(
            "{shape}: {counted} B of line counted per record, {on_disk} B on disk per record \
             (x{:.2}); at {CAP_PER_SECOND}/s that is {:.1} GiB a day on disk, {:.1} GiB a day counted \
             by the 2 GiB warning, {:.0} GiB in 90 days",
            on_disk as f64 / counted as f64,
            per_day as f64 / 1_073_741_824.0,
            (counted * CAP_PER_SECOND * 86_400) as f64 / 1_073_741_824.0,
            (per_day * 90) as f64 / 1_073_741_824.0,
        );
        shapes.push((shape, on_disk));
    }
    for (shape, on_disk) in shapes {
        assert!(on_disk > 0, "{shape}: nothing was written");
    }
}
