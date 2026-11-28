// SPDX-License-Identifier: AGPL-3.0-or-later
use std::sync::atomic::{AtomicUsize, Ordering};

use puddle_agent_proto::resolve::StandInReason;
use puddle_test_kit::{eventually, eventually_async};

use super::*;
use crate::capture::table::in_range;
use crate::capture::wire::tests::query_bytes;

const WAIT: Duration = Duration::from_secs(5);

/// What the fake host knows: a function from a query to its answer, counting the asks.
struct Fake<F> {
    f: F,
    asks: AtomicUsize,
}

impl<F> Fake<F> {
    fn new(f: F) -> Self {
        Self {
            f,
            asks: AtomicUsize::new(0),
        }
    }
}

impl<F, Fut> Ask for Arc<Fake<F>>
where
    F: Fn(ResolveQuery) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<ResolveAnswer, ResolveError>> + Send + 'static,
{
    fn ask(
        &self,
        query: ResolveQuery,
    ) -> impl Future<Output = Result<ResolveAnswer, ResolveError>> + Send {
        self.asks.fetch_add(1, Ordering::SeqCst);
        (self.f)(query)
    }
}

fn stand_in() -> ResolveAnswer {
    ResolveAnswer::StandIn {
        why: StandInReason::Resolves,
        ttl: 60,
    }
}

type Handle<F> = Arc<Fake<F>>;

fn stub_with<F, Fut>(f: F, limits: Limits) -> (Stub<Handle<F>>, Handle<F>, StandIns)
where
    F: Fn(ResolveQuery) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<ResolveAnswer, ResolveError>> + Send + 'static,
{
    let fake = Arc::new(Fake::new(f));
    let table = StandIns::in_memory();
    (
        Stub::new(Arc::clone(&fake), table.clone(), limits),
        fake,
        table,
    )
}

type Ready = std::future::Ready<Result<ResolveAnswer, ResolveError>>;
type Fixed = Handle<Box<dyn Fn(ResolveQuery) -> Ready + Send + Sync>>;

/// A host that gives `answer` to every question.
fn answers_with(answer: ResolveAnswer) -> (Stub<Fixed>, Fixed, StandIns) {
    let fixed: Box<dyn Fn(ResolveQuery) -> Ready + Send + Sync> =
        Box::new(move |_| std::future::ready(Ok(answer.clone())));
    stub_with(fixed, Limits::default())
}

/// A decoded response.
#[derive(Debug)]
struct Response {
    id: u16,
    flags: u16,
    questions: u16,
    answers: Vec<Rr>,
    additional: Vec<Rr>,
    len: usize,
}

#[derive(Debug, Clone)]
struct Rr {
    owner: String,
    rtype: u16,
    ttl: u32,
    rdata: Vec<u8>,
    message: Vec<u8>,
    rdata_at: usize,
}

impl Response {
    fn rcode(&self) -> u16 {
        self.flags & 0xF
    }
    fn truncated(&self) -> bool {
        self.flags & 0x200 != 0
    }
}

fn word(m: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([m[at], m[at + 1]])
}

/// Reads a name at `at` (following pointers); returns it and the offset after it in place.
fn read_name(m: &[u8], mut at: usize) -> (String, usize) {
    let mut name = String::new();
    let mut after = None;
    loop {
        let len = usize::from(m[at]);
        if len == 0 {
            at += 1;
            break;
        }
        if len & 0xC0 == 0xC0 {
            let target = usize::from(word(m, at) & 0x3FFF);
            after.get_or_insert(at + 2);
            at = target;
            continue;
        }
        if !name.is_empty() {
            name.push('.');
        }
        name.push_str(std::str::from_utf8(&m[at + 1..at + 1 + len]).unwrap());
        at += 1 + len;
    }
    (name, after.unwrap_or(at))
}

fn decode(m: &[u8]) -> Response {
    let (qd, an, ar) = (word(m, 4), word(m, 6), word(m, 10));
    let mut at = 12;
    for _ in 0..qd {
        let (_, next) = read_name(m, at);
        at = next + 4;
    }
    let rrs = |n: u16, at: &mut usize| -> Vec<Rr> {
        (0..n)
            .map(|_| {
                let (owner, next) = read_name(m, *at);
                let rtype = word(m, next);
                let ttl = u32::from_be_bytes([m[next + 4], m[next + 5], m[next + 6], m[next + 7]]);
                let rdlen = usize::from(word(m, next + 8));
                let rdata_at = next + 10;
                *at = rdata_at + rdlen;
                Rr {
                    owner,
                    rtype,
                    ttl,
                    rdata: m[rdata_at..rdata_at + rdlen].to_vec(),
                    message: m.to_vec(),
                    rdata_at,
                }
            })
            .collect()
    };
    let answers = rrs(an, &mut at);
    let additional = rrs(ar, &mut at);
    Response {
        id: word(m, 0),
        flags: word(m, 2),
        questions: qd,
        answers,
        additional,
        len: m.len(),
    }
}

impl Rr {
    fn ip(&self) -> Ipv4Addr {
        Ipv4Addr::new(self.rdata[0], self.rdata[1], self.rdata[2], self.rdata[3])
    }
    /// The target name of an SRV (after 6 bytes) or MX (after 2) record.
    fn target(&self, skip: usize) -> String {
        read_name(&self.message, self.rdata_at + skip).0
    }
}

async fn ask_stub<A: Ask>(stub: &Stub<A>, name: &str, qtype: u16) -> Response {
    let reply = stub
        .answer(&query_bytes(0x4242, name, qtype, None), true)
        .await
        .expect("an answer");
    let r = decode(&reply);
    assert_eq!(r.id, 0x4242);
    assert_eq!(r.questions, 1);
    r
}

#[tokio::test]
async fn an_address_query_gets_a_stand_in_from_the_range_and_the_same_one_every_time() {
    let (stub, fake, table) = answers_with(stand_in());
    let first = ask_stub(&stub, "api.example.com", TYPE_A).await;
    assert_eq!(first.rcode(), 0);
    assert_eq!(first.answers.len(), 1);
    let ip = first.answers[0].ip();
    assert!(in_range(ip));
    assert_eq!(first.answers[0].ttl, 60);
    assert_eq!(first.answers[0].owner, "api.example.com");
    assert_eq!(table.name_for(ip).as_deref(), Some("api.example.com"));
    let second = ask_stub(&stub, "API.example.com", TYPE_A).await;
    assert_eq!(second.answers[0].ip(), ip);
    let other = ask_stub(&stub, "other.example.com", TYPE_A).await;
    assert_ne!(other.answers[0].ip(), ip);
    assert_eq!(
        fake.asks.load(Ordering::SeqCst),
        2,
        "the second query was cached"
    );
}

#[tokio::test(start_paused = true)]
async fn a_hundred_parallel_queries_for_one_name_cost_one_lookup() {
    let (stub, fake, _) = stub_with(
        |_| async {
            // Virtual time: the lookup stays open until every query has joined it.
            tokio::time::sleep(Duration::from_millis(50)).await;
            Ok(stand_in())
        },
        Limits::default(),
    );
    let mut set = JoinSet::new();
    for _ in 0..100 {
        let stub = stub.clone();
        set.spawn(async move { ask_stub(&stub, "busy.example.com", TYPE_A).await.answers[0].ip() });
    }
    let mut seen = std::collections::HashSet::new();
    while let Some(ip) = set.join_next().await {
        seen.insert(ip.unwrap());
    }
    assert_eq!(seen.len(), 1);
    assert_eq!(fake.asks.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn an_answer_is_kept_for_the_time_the_host_said_and_asked_again_after() {
    let (stub, fake, _) = stub_with(
        |_| std::future::ready(Ok(ResolveAnswer::NoSuchName { ttl: 20 })),
        Limits::default(),
    );
    assert_eq!(ask_stub(&stub, "gone.example.com", TYPE_A).await.rcode(), 3);
    tokio::time::advance(Duration::from_secs(19)).await;
    assert_eq!(ask_stub(&stub, "gone.example.com", TYPE_A).await.rcode(), 3);
    assert_eq!(fake.asks.load(Ordering::SeqCst), 1);
    tokio::time::advance(Duration::from_secs(2)).await;
    assert_eq!(ask_stub(&stub, "gone.example.com", TYPE_A).await.rcode(), 3);
    assert_eq!(fake.asks.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn the_stand_in_ttl_given_to_the_guest_is_at_most_a_minute() {
    let (stub, _, _) = answers_with(ResolveAnswer::StandIn {
        why: StandInReason::ViaUpstream,
        ttl: 3600,
    });
    let r = ask_stub(&stub, "long.example.com", TYPE_A).await;
    assert_eq!(r.answers[0].ttl, 60);
    let (stub, _, _) = answers_with(ResolveAnswer::StandIn {
        why: StandInReason::Resolves,
        ttl: 0,
    });
    assert_eq!(
        ask_stub(&stub, "zero.example.com", TYPE_A).await.answers[0].ttl,
        1
    );
}

#[tokio::test]
async fn aaaa_is_nodata_without_asking_and_nxdomain_when_the_name_is_known_missing() {
    let (stub, fake, _) = answers_with(ResolveAnswer::NoSuchName { ttl: 30 });
    let r = ask_stub(&stub, "gone.example.com", 28).await;
    assert_eq!((r.rcode(), r.answers.len()), (0, 0));
    assert_eq!(fake.asks.load(Ordering::SeqCst), 0);
    // Once the A lookup found nothing, the other types agree.
    assert_eq!(ask_stub(&stub, "gone.example.com", TYPE_A).await.rcode(), 3);
    assert_eq!(ask_stub(&stub, "gone.example.com", 28).await.rcode(), 3);
    assert_eq!(ask_stub(&stub, "gone.example.com", 65).await.rcode(), 3);
    assert_eq!(fake.asks.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn names_that_can_never_be_a_connect_target_are_nxdomain_without_asking() {
    let (stub, fake, _) = answers_with(stand_in());
    for name in ["4.3.2.1.in-addr.arpa", "1.2.3.4"] {
        for qtype in [TYPE_A, 28, TYPE_SRV, TYPE_TXT, 12] {
            let r = ask_stub(&stub, name, qtype).await;
            assert_eq!(r.rcode(), 3, "{name} type {qtype}");
        }
    }
    assert_eq!(fake.asks.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn odd_names_and_foreign_classes_never_reach_the_host() {
    let (stub, fake, _) = answers_with(stand_in());
    let mut odd = query_bytes(1, "abcd.example.com", TYPE_A, None);
    odd[13] = 0x1B;
    let r = decode(&stub.answer(&odd, true).await.unwrap());
    assert_eq!(r.rcode(), 3);
    // A CHAOS-class query.
    let mut chaos = query_bytes(2, "version.bind", 16, None);
    let n = chaos.len();
    chaos[n - 1] = 3;
    let r = decode(&stub.answer(&chaos, true).await.unwrap());
    assert_eq!(r.rcode(), 5);
    assert_eq!(fake.asks.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn garbage_is_dropped_or_answered_with_an_error_and_never_asks() {
    let (stub, fake, _) = answers_with(stand_in());
    assert!(stub.answer(&[1, 2, 3], true).await.is_none());
    let mut response = query_bytes(1, "example.com", TYPE_A, None);
    response[2] |= 0x80;
    assert!(stub.answer(&response, true).await.is_none());
    let mut two = query_bytes(9, "example.com", TYPE_A, None);
    two[5] = 2;
    let r = decode(&stub.answer(&two, true).await.unwrap());
    assert_eq!((r.id, r.rcode()), (9, 1));
    assert_eq!(fake.asks.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_host_that_cannot_answer_is_servfail_and_not_remembered() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&calls);
    let (stub, fake, _) = stub_with(
        move |_| {
            let n = seen.fetch_add(1, Ordering::SeqCst);
            std::future::ready(match n {
                0 => Ok(ResolveAnswer::Unavailable),
                1 => Err(ResolveError::NoAnswer),
                2 => Ok(ResolveAnswer::Unknown),
                _ => Ok(stand_in()),
            })
        },
        Limits::default(),
    );
    for _ in 0..3 {
        assert_eq!(
            ask_stub(&stub, "flaky.example.com", TYPE_A).await.rcode(),
            2
        );
    }
    let r = ask_stub(&stub, "flaky.example.com", TYPE_A).await;
    assert_eq!(r.rcode(), 0, "the next try asks again");
    assert_eq!(fake.asks.load(Ordering::SeqCst), 4);
}

#[tokio::test(start_paused = true)]
async fn a_host_that_never_answers_is_servfail_after_the_limit() {
    let (stub, _, _) = stub_with(
        |_| std::future::pending::<Result<ResolveAnswer, ResolveError>>(),
        Limits {
            ask_timeout: Duration::from_secs(2),
            ..Limits::default()
        },
    );
    assert_eq!(ask_stub(&stub, "hang.example.com", TYPE_A).await.rcode(), 2);
}

#[tokio::test(start_paused = true)]
async fn lookups_on_the_host_never_exceed_the_limit_and_a_query_that_waits_too_long_is_dropped() {
    struct Slot(Arc<AtomicUsize>);
    impl Drop for Slot {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }
    let (current, peak) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let (c, p) = (Arc::clone(&current), Arc::clone(&peak));
    // One slot and a host that takes 1.5 s: the first query is answered at 1.5 s, the second gets
    // the slot then and is answered at 3 s, and the third has waited its 2 s by then.
    let (stub, fake, _) = stub_with(
        move |_| {
            let now = c.fetch_add(1, Ordering::SeqCst) + 1;
            p.fetch_max(now, Ordering::SeqCst);
            let slot = Slot(Arc::clone(&c));
            async move {
                let _slot = slot;
                // The host's lookup time, on virtual time.
                tokio::time::sleep(Duration::from_millis(1500)).await;
                Ok(stand_in())
            }
        },
        Limits {
            ask_timeout: Duration::from_secs(2),
            max_asks: 1,
            ..Limits::default()
        },
    );
    let mut set = JoinSet::new();
    // flake-ok: spawns three queries staggered by a millisecond to fix their queue order, nothing is waited for
    for i in 0..3 {
        let stub = stub.clone();
        set.spawn(async move {
            // A pause per query pins the order in which they queue for the slot.
            tokio::time::sleep(Duration::from_millis(i)).await;
            let message = query_bytes(0x4242, &format!("n{i}.example.com"), TYPE_A, None);
            (
                i,
                stub.answer(&message, true)
                    .await
                    .map(|r| decode(&r).rcode()),
            )
        });
    }
    let mut outcomes = Vec::new();
    while let Some(done) = set.join_next().await {
        outcomes.push(done.unwrap());
    }
    outcomes.sort_unstable();
    assert_eq!(outcomes, vec![(0, Some(0)), (1, Some(0)), (2, None)]);
    assert_eq!(peak.load(Ordering::SeqCst), 1);
    assert_eq!(
        fake.asks.load(Ordering::SeqCst),
        2,
        "the dropped one was never asked"
    );
}

#[tokio::test(start_paused = true)]
async fn a_query_that_joins_a_lookup_about_to_give_up_waits_for_a_new_one() {
    // One slot, a host that takes 1.9 s, a 2 s limit. Names a and b take the slot in turn (a is
    // answered at 1.9 s, b at 3.8 s). The lookup of c, asked at 0.1 s, gives up at 2.1 s. The
    // second query for c comes at 1.9 s and joins it, but has waited only 0.2 s by then, so it
    // starts its own lookup instead of being dropped with it.
    let (stub, fake, _) = stub_with(
        move |_| async {
            // The host's lookup time, on virtual time.
            tokio::time::sleep(Duration::from_millis(1900)).await;
            Ok(stand_in())
        },
        Limits {
            ask_timeout: Duration::from_secs(2),
            max_asks: 1,
            ..Limits::default()
        },
    );
    let query_after = |name: &'static str, at_ms: u64| {
        let stub = stub.clone();
        tokio::spawn(async move {
            // Virtual time: each query is sent at its own instant.
            tokio::time::sleep(Duration::from_millis(at_ms)).await;
            let message = query_bytes(0x4242, name, TYPE_A, None);
            stub.answer(&message, true)
                .await
                .map(|r| decode(&r).rcode())
        })
    };
    let queries = [
        query_after("a.example.com", 0),
        query_after("b.example.com", 1),
        query_after("c.example.com", 100),
        query_after("c.example.com", 1900),
    ];
    let mut answers = Vec::new();
    for query in queries {
        answers.push(query.await.unwrap());
    }
    assert_eq!(answers, vec![Some(0), Some(0), None, Some(0)]);
    assert_eq!(fake.asks.load(Ordering::SeqCst), 3, "a, b and the second c");
}

#[tokio::test]
async fn a_host_answer_of_the_wrong_kind_is_not_passed_on() {
    let (stub, _, _) = answers_with(ResolveAnswer::Records {
        records: vec![],
        ttl: 30,
    });
    assert_eq!(ask_stub(&stub, "x.example.com", TYPE_A).await.rcode(), 2);
    let (stub, _, _) = answers_with(stand_in());
    assert_eq!(ask_stub(&stub, "x.example.com", TYPE_TXT).await.rcode(), 2);
}

#[tokio::test]
async fn srv_records_come_with_a_stand_in_for_each_target() {
    let (stub, fake, table) = answers_with(ResolveAnswer::Records {
        records: vec![
            HostRecord::Srv {
                priority: 0,
                weight: 5,
                port: 27017,
                target: "shard0.db.example.net".into(),
            },
            HostRecord::Srv {
                priority: 0,
                weight: 5,
                port: 27017,
                target: "shard1.db.example.net".into(),
            },
            HostRecord::Srv {
                priority: 1,
                weight: 1,
                port: 27017,
                target: "shard0.db.example.net".into(),
            },
            // Not plain: dropped. A TXT in an SRV answer: dropped. A single label is a target like any other.
            HostRecord::Srv {
                priority: 0,
                weight: 1,
                port: 1,
                target: "bad name.example".into(),
            },
            HostRecord::Txt {
                strings: vec!["x".into()],
            },
            HostRecord::Srv {
                priority: 2,
                weight: 1,
                port: 1,
                target: "single-label".into(),
            },
        ],
        ttl: 30,
    });
    let r = ask_stub(&stub, "_mongodb._tcp.db.example.net", TYPE_SRV).await;
    assert_eq!(r.rcode(), 0);
    assert_eq!(r.answers.len(), 4);
    assert_eq!(r.answers[0].rtype, TYPE_SRV);
    assert_eq!(word(&r.answers[0].rdata, 4), 27017);
    assert_eq!(r.answers[0].target(6), "shard0.db.example.net");
    assert_eq!(r.answers[1].target(6), "shard1.db.example.net");
    assert_eq!(r.answers[3].target(6), "single-label");
    // One stand-in per distinct target, in the additional section.
    assert_eq!(r.additional.len(), 3);
    for extra in &r.additional {
        assert_eq!(extra.rtype, TYPE_A);
        assert_eq!(
            table.name_for(extra.ip()).as_deref(),
            Some(extra.owner.as_str())
        );
    }
    // The client's next query for a target is answered with the same stand-in, from the table.
    let a = ask_stub(&stub, "shard1.db.example.net", TYPE_A).await;
    assert_eq!(
        a.rcode(),
        2,
        "the fake host only knows records, so an A lookup fails"
    );
    assert_eq!(
        table.address_for("shard1.db.example.net").unwrap(),
        r.additional[1].ip()
    );
    assert_eq!(fake.asks.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn txt_and_mx_records_are_encoded_and_other_types_in_the_answer_are_dropped() {
    let (stub, _, _) = answers_with(ResolveAnswer::Records {
        records: vec![
            HostRecord::Txt {
                strings: vec!["authSource=admin".into(), "replicaSet=rs0".into()],
            },
            HostRecord::Mx {
                preference: 10,
                exchange: "mx.example.com".into(),
            },
        ],
        ttl: 30,
    });
    let txt = ask_stub(&stub, "db.example.com", TYPE_TXT).await;
    assert_eq!(txt.answers.len(), 1);
    assert_eq!(txt.answers[0].rdata[0], 16);
    assert!(txt.additional.is_empty());
    let mx = ask_stub(&stub, "db.example.com", TYPE_MX).await;
    assert_eq!(mx.answers.len(), 1);
    assert_eq!(word(&mx.answers[0].rdata, 0), 10);
    assert_eq!(mx.answers[0].target(2), "mx.example.com");
    assert_eq!(mx.additional.len(), 1);
}

#[tokio::test]
async fn nodata_and_nxdomain_for_records_pass_through() {
    let (stub, _, _) = answers_with(ResolveAnswer::NoData { ttl: 20 });
    let r = ask_stub(&stub, "db.example.com", TYPE_SRV).await;
    assert_eq!((r.rcode(), r.answers.len()), (0, 0));
    let (stub, _, _) = answers_with(ResolveAnswer::NoSuchName { ttl: 20 });
    assert_eq!(ask_stub(&stub, "db.example.com", TYPE_TXT).await.rcode(), 3);
    // A record answer whose every record is unusable is NODATA.
    let (stub, _, _) = answers_with(ResolveAnswer::Records {
        records: vec![HostRecord::Txt { strings: vec![] }],
        ttl: 20,
    });
    let r = ask_stub(&stub, "db.example.com", TYPE_SRV).await;
    assert_eq!((r.rcode(), r.answers.len()), (0, 0));
}

#[tokio::test]
async fn the_cache_does_not_grow_past_its_limit() {
    let (stub, fake, _) = stub_with(
        |_| std::future::ready(Ok(stand_in())),
        Limits {
            cache_entries: 8,
            ..Limits::default()
        },
    );
    for i in 0..100 {
        ask_stub(&stub, &format!("n{i}.example.com"), TYPE_A).await;
    }
    assert_eq!(fake.asks.load(Ordering::SeqCst), 100);
    let held = stub.shared.cache.lock().unwrap().map.len();
    assert!(held <= 8, "{held}");
}

#[tokio::test]
async fn when_every_stand_in_is_in_use_the_answer_is_servfail() {
    let table = StandIns::open_with_capacity(None, 1);
    let fake = Arc::new(Fake::new(|_| std::future::ready(Ok(stand_in()))));
    let stub = Stub::new(Arc::clone(&fake), table.clone(), Limits::default());
    let first = ask_stub(&stub, "a.example.com", TYPE_A).await;
    let _held = table.hold(first.answers[0].ip()).unwrap();
    assert_eq!(ask_stub(&stub, "b.example.com", TYPE_A).await.rcode(), 2);
}

mod sockets;
