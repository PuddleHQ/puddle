// SPDX-License-Identifier: GPL-3.0-or-later
//! The stub DNS server.
//!
//! Tools that ignore the proxy settings resolve a name and connect to the answer. The stub answers
//! every address query for a name the host doesn't rule out with a stand-in address from
//! [`super::table`], and the capture rules send the connection to the agent, which sends the
//! name to the host's proxy like any other client of it.
//!
//! What the stub answers itself, without asking the host:
//!
//! - names that can never be a connect target ([`names::is_local_only`]): `NXDOMAIN`;
//! - `AAAA` and every other type that isn't forwarded: `NODATA` (so dual-stack clients use the
//!   stand-in at once), or `NXDOMAIN` when the name is already known not to exist;
//! - anything it already learned recently (the cache, for the time the host said).
//!
//! The rest is one lookup on a `resolve` stream ([`puddle_agent_proto::resolve`]); concurrent
//! queries for the same name share one lookup. The host decides with the sandbox's rules, and a
//! name it doesn't allow gets a stand-in without being looked up anywhere.

use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use futures_util::FutureExt as _;
use futures_util::future::{BoxFuture, Shared};
use puddle_agent_proto::resolve::{
    self, Record as HostRecord, RecordType, ResolveAnswer, ResolveError, ResolveQuery,
};
use puddle_agent_proto::{AgentMessage, StubDnsTrouble};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, UdpSocket};
use tokio::sync::{Semaphore, mpsc};
use tokio::task::JoinSet;
use tokio::time::Instant;

use super::names;
use super::table::StandIns;
use super::wire::{
    self, CLASS_IN, MAX_QUERY, QueryName, Rcode, Rdata, Record, Reject, TYPE_A, TYPE_MX, TYPE_SRV,
    TYPE_TXT,
};
use crate::upstream::Upstream;

/// Longest time (seconds) a stand-in answer may be cached by the guest.
const GUEST_TTL_CAP: u32 = 60;
/// The range a host-given time to live is kept in for the stub's own cache (seconds).
const CACHE_TTL: (u32, u32) = (1, 300);

/// Limits of the stub. The defaults suit a real guest; tests shorten them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// How long one lookup on the host may take (and how long a query waits for a free slot).
    pub ask_timeout: Duration,
    /// Lookups on the host in flight at once.
    pub max_asks: usize,
    /// Answers kept (the cache is emptied of expired ones, then cleared, when full).
    pub cache_entries: usize,
    /// UDP queries handled at once; more are dropped (the client retries).
    pub max_udp_queries: usize,
    /// TCP connections open at once; more are closed.
    pub max_tcp_connections: usize,
    /// How long a TCP connection may sit idle.
    pub tcp_idle: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            ask_timeout: Duration::from_secs(5),
            max_asks: 128,
            cache_entries: 4096,
            max_udp_queries: 1024,
            max_tcp_connections: 64,
            tcp_idle: Duration::from_secs(10),
        }
    }
}

/// How the stub asks the host about a name: one lookup per call.
pub trait Ask: Send + Sync + 'static {
    /// Asks the host.
    ///
    /// # Errors
    ///
    /// [`ResolveError`] when there is no answer (stream failed, timed out, bad answer).
    fn ask(
        &self,
        query: ResolveQuery,
    ) -> impl Future<Output = Result<ResolveAnswer, ResolveError>> + Send;
}

/// Asks the host over a fresh `resolve` stream of the agent's sessions.
#[derive(Debug)]
pub struct HostAsk {
    upstream: Arc<Upstream>,
    limit: Duration,
}

impl HostAsk {
    /// Asks through `upstream`, waiting at most `limit` for each answer.
    #[must_use]
    pub fn new(upstream: Arc<Upstream>, limit: Duration) -> Self {
        Self { upstream, limit }
    }
}

impl Ask for HostAsk {
    async fn ask(&self, query: ResolveQuery) -> Result<ResolveAnswer, ResolveError> {
        let stream = tokio::time::timeout(self.limit, self.upstream.open())
            .await
            .map_err(|_| ResolveError::Timeout(self.limit))??;
        resolve::ask(stream, &query, self.limit).await
    }
}

type Key = (String, RecordType);

/// Answers the host gave, kept for the time it said.
#[derive(Debug)]
struct Cache {
    map: HashMap<Key, (ResolveAnswer, Instant)>,
    capacity: usize,
}

impl Cache {
    fn get(&self, key: &Key, now: Instant) -> Option<ResolveAnswer> {
        let (answer, expires) = self.map.get(key)?;
        (now < *expires).then(|| answer.clone())
    }

    fn put(&mut self, key: Key, answer: ResolveAnswer, ttl: u32, now: Instant) {
        if self.map.len() >= self.capacity {
            self.map.retain(|_, (_, expires)| now < *expires);
            if self.map.len() >= self.capacity {
                self.map.clear();
            }
        }
        let ttl = ttl.clamp(CACHE_TTL.0, CACHE_TTL.1);
        self.map
            .insert(key, (answer, now + Duration::from_secs(u64::from(ttl))));
    }
}

/// One lookup on the host that every query for the same thing shares. `None`: it never got a
/// slot, so nobody asked the host.
type Flight = Shared<BoxFuture<'static, Option<ResolveAnswer>>>;

type Reply = (Rcode, Vec<Record>, Vec<Record>);

struct State<A> {
    ask: A,
    stand_ins: StandIns,
    cache: Mutex<Cache>,
    inflight: Mutex<HashMap<Key, Flight>>,
    asks: Semaphore,
    limits: Limits,
    /// What went wrong, counted per kind for the host.
    tally: Mutex<Tally>,
    /// The control stream's queue; `None` tells the host nothing.
    reports: Option<mpsc::Sender<AgentMessage>>,
}

impl<A> State<A> {
    /// Counts one `kind` of trouble and reports it to the host when a report is due.
    fn trouble(&self, kind: StubDnsTrouble) {
        let Some(reports) = &self.reports else { return };
        let due = self
            .tally
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .count(kind, Instant::now());
        if let Some(count) = due
            && reports
                .try_send(AgentMessage::stub_dns(kind, count))
                .is_err()
        {
            tracing::debug!(
                ?kind,
                count,
                "stub DNS report not queued: the host isn't taking reports"
            );
        }
    }
}

/// Counts each kind of trouble and says when to report it: the first one at once, then at most one
/// report per [`Tally::EVERY`] with the number since the last.
#[derive(Debug, Default)]
pub(crate) struct Tally {
    kinds: HashMap<StubDnsTrouble, (Option<Instant>, u32)>,
}

impl Tally {
    /// Shortest time between two reports of one kind.
    pub(crate) const EVERY: Duration = Duration::from_secs(10);

    /// Counts one `kind`; the number to report when a report is due now.
    pub(crate) fn count(&mut self, kind: StubDnsTrouble, now: Instant) -> Option<u32> {
        let (since, count) = self.kinds.entry(kind).or_default();
        *count = count.saturating_add(1);
        if since.is_some_and(|at| now.duration_since(at) < Self::EVERY) {
            return None;
        }
        *since = Some(now);
        Some(std::mem::take(count))
    }
}

/// The stub's brain: turns a DNS query into a DNS answer. Cheap to clone.
pub struct Stub<A> {
    shared: Arc<State<A>>,
}

impl<A> Clone for Stub<A> {
    fn clone(&self) -> Self {
        Self {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl<A: Ask> Stub<A> {
    /// A stub that asks `ask` and hands out addresses from `stand_ins`.
    #[must_use]
    pub fn new(ask: A, stand_ins: StandIns, limits: Limits) -> Self {
        Self::reporting(ask, stand_ins, limits, None)
    }

    /// [`Stub::new`] that also tells the host, through `reports` (the control stream's queue),
    /// what it turns away or cannot carry.
    #[must_use]
    pub fn reporting(
        ask: A,
        stand_ins: StandIns,
        limits: Limits,
        reports: Option<mpsc::Sender<AgentMessage>>,
    ) -> Self {
        Self {
            shared: Arc::new(State {
                ask,
                stand_ins,
                cache: Mutex::new(Cache {
                    map: HashMap::new(),
                    capacity: limits.cache_entries.max(1),
                }),
                inflight: Mutex::new(HashMap::new()),
                asks: Semaphore::new(limits.max_asks.max(1)),
                limits,
                tally: Mutex::new(Tally::default()),
                reports,
            }),
        }
    }

    /// Answers one DNS message from the guest, or `None` for a message to drop: a response, one
    /// too short to be a query, or a query that waited its whole time for a lookup slot (it was
    /// never tried, so the guest asks again instead of taking a failure for the name). `udp`
    /// limits the answer's size.
    pub async fn answer(&self, message: &[u8], udp: bool) -> Option<Vec<u8>> {
        let query = match wire::parse_query(message) {
            Ok(query) => query,
            Err(Reject::Drop) => return None,
            Err(Reject::Answer {
                id,
                rcode,
                recursion_desired,
            }) => return Some(wire::error_response(id, rcode, recursion_desired)),
        };
        let (rcode, answers, additional) = self.respond(&query).await?;
        Some(wire::respond(&query, rcode, &answers, &additional, udp))
    }

    async fn respond(&self, query: &wire::Query) -> Option<Reply> {
        let name = match &query.name {
            QueryName::Plain(name) if names::is_plain(name) || name.is_empty() => name,
            _ => return Some((Rcode::NxDomain, vec![], vec![])),
        };
        if query.qclass != CLASS_IN {
            return Some((Rcode::Refused, vec![], vec![]));
        }
        if names::is_local_only(name) {
            return Some((Rcode::NxDomain, vec![], vec![]));
        }
        match query.qtype {
            TYPE_A => self.address(name).await,
            TYPE_SRV => self.records(name, RecordType::Srv).await,
            TYPE_TXT => self.records(name, RecordType::Txt).await,
            TYPE_MX => self.records(name, RecordType::Mx).await,
            _ => {
                // `AAAA` and the rest: nothing to say about types we don't forward, unless the
                // name is already known not to exist.
                let known_missing = matches!(
                    self.cached(name, RecordType::A),
                    Some(ResolveAnswer::NoSuchName { .. })
                );
                let rcode = if known_missing {
                    Rcode::NxDomain
                } else {
                    Rcode::NoError
                };
                Some((rcode, vec![], vec![]))
            }
        }
    }

    async fn address(&self, name: &str) -> Option<Reply> {
        let reply = match self.lookup(name, RecordType::A).await? {
            ResolveAnswer::StandIn { ttl, .. } => match self.shared.stand_ins.address_for(name) {
                Ok(ip) => (Rcode::NoError, vec![stand_in_record(None, ip, ttl)], vec![]),
                Err(err) => {
                    tracing::warn!(%name, error = %err, "no stand-in address left");
                    self.shared.trouble(StubDnsTrouble::NoStandInAddress);
                    (Rcode::ServFail, vec![], vec![])
                }
            },
            ResolveAnswer::NoSuchName { .. } => (Rcode::NxDomain, vec![], vec![]),
            ResolveAnswer::NoData { .. } => (Rcode::NoError, vec![], vec![]),
            _ => (Rcode::ServFail, vec![], vec![]),
        };
        Some(reply)
    }

    async fn records(&self, name: &str, rtype: RecordType) -> Option<Reply> {
        let reply = match self.lookup(name, rtype).await? {
            ResolveAnswer::Records { records, ttl } => {
                let mut answers = Vec::new();
                let mut additional: Vec<Record> = Vec::new();
                let mut left_out = 0_usize;
                for record in records {
                    let Some((data, target)) = convert(rtype, record) else {
                        left_out += 1;
                        continue;
                    };
                    answers.push(Record {
                        owner: None,
                        ttl: ttl.min(GUEST_TTL_CAP),
                        data,
                    });
                    // A target gets a stand-in of its own, so the client's next step (connecting
                    // to it) needs no lookup of the name it was just given.
                    let known = additional
                        .iter()
                        .any(|r| r.owner.as_deref() == target.as_deref());
                    if let Some(target) = target
                        && !known
                        && !names::is_local_only(&target)
                        && let Ok(ip) = self.shared.stand_ins.address_for(&target)
                    {
                        additional.push(stand_in_record(Some(target), ip, ttl));
                    }
                }
                if left_out > 0 {
                    tracing::warn!(%name, ?rtype, left_out, "records the stub cannot carry (other types, or a target that is not a plain host name) were left out of the answer");
                    self.shared.trouble(StubDnsTrouble::RecordsLeftOut);
                }
                if answers.is_empty() {
                    (Rcode::NoError, vec![], vec![])
                } else {
                    (Rcode::NoError, answers, additional)
                }
            }
            ResolveAnswer::NoSuchName { .. } => (Rcode::NxDomain, vec![], vec![]),
            ResolveAnswer::NoData { .. } => (Rcode::NoError, vec![], vec![]),
            _ => (Rcode::ServFail, vec![], vec![]),
        };
        Some(reply)
    }

    fn cached(&self, name: &str, rtype: RecordType) -> Option<ResolveAnswer> {
        let cache = self
            .shared
            .cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        cache.get(&(name.to_owned(), rtype), Instant::now())
    }

    /// What the host says about `name`: from the cache, else from one lookup shared by every
    /// query for the same thing that is in flight. `None` when no lookup slot came free within
    /// one `ask_timeout` of this query's arrival.
    async fn lookup(&self, name: &str, rtype: RecordType) -> Option<ResolveAnswer> {
        let key = (name.to_owned(), rtype);
        let arrived = Instant::now();
        loop {
            if let Some(hit) = self.cached(name, rtype) {
                return Some(hit);
            }
            let flight = self
                .shared
                .inflight
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(key.clone())
                .or_insert_with(|| {
                    let shared = Arc::clone(&self.shared);
                    let key = key.clone();
                    async move {
                        let answer = shared.ask_host(key.clone()).await;
                        // Gone before anyone sees the result, so a query that gets `None` and
                        // tries again starts a new lookup instead of joining this one.
                        shared
                            .inflight
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .remove(&key);
                        answer
                    }
                    .boxed()
                    .shared()
                })
                .clone();
            if let Some(answer) = flight.await {
                return Some(answer);
            }
            // The lookup it joined may have been waiting for a slot since before this query
            // came; the query's own wait is not over until its own time is.
            if arrived.elapsed() >= self.shared.limits.ask_timeout {
                return None;
            }
        }
    }
}

impl<A: Ask> State<A> {
    /// One lookup on the host, bounded in number and time. A failure is `Unavailable` and is not
    /// kept. `None` when the wait for a slot ran out: a burst deeper than the slots can serve in
    /// one `ask_timeout` leaves its tail unasked, and that is no verdict on the name.
    async fn ask_host(&self, key: Key) -> Option<ResolveAnswer> {
        let limit = self.limits.ask_timeout;
        let Ok(Ok(_slot)) = tokio::time::timeout(limit, self.asks.acquire()).await else {
            tracing::warn!(name = %key.0, "no free lookup slot in time; query dropped, the client asks again");
            self.trouble(StubDnsTrouble::NoLookupSlot);
            return None;
        };
        let query = ResolveQuery::new(key.0.clone(), key.1);
        let answer = match tokio::time::timeout(limit, self.ask.ask(query)).await {
            Ok(Ok(answer)) => answer,
            Ok(Err(err)) => {
                tracing::warn!(name = %key.0, error = %err, "lookup on the host failed; answered SERVFAIL");
                self.trouble(StubDnsTrouble::LookupFailed);
                return Some(ResolveAnswer::Unavailable);
            }
            Err(_) => {
                tracing::warn!(name = %key.0, "lookup on the host timed out; answered SERVFAIL");
                self.trouble(StubDnsTrouble::LookupTimedOut);
                return Some(ResolveAnswer::Unavailable);
            }
        };
        let answer = fit_to(key.1, answer);
        let ttl = match &answer {
            ResolveAnswer::StandIn { ttl, .. }
            | ResolveAnswer::NoSuchName { ttl }
            | ResolveAnswer::NoData { ttl }
            | ResolveAnswer::Records { ttl, .. } => *ttl,
            _ => return Some(answer),
        };
        self.cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .put(key, answer.clone(), ttl, Instant::now());
        Some(answer)
    }
}

/// An answer of a kind the question can't have (a stand-in for a `TXT` query, records for an `A`
/// query) is a failure, not something to pass on.
fn fit_to(rtype: RecordType, answer: ResolveAnswer) -> ResolveAnswer {
    match (rtype, &answer) {
        (
            RecordType::A,
            ResolveAnswer::StandIn { .. }
            | ResolveAnswer::NoSuchName { .. }
            | ResolveAnswer::NoData { .. },
        )
        | (
            RecordType::Srv | RecordType::Txt | RecordType::Mx,
            ResolveAnswer::Records { .. }
            | ResolveAnswer::NoSuchName { .. }
            | ResolveAnswer::NoData { .. },
        ) => answer,
        _ => ResolveAnswer::Unavailable,
    }
}

fn stand_in_record(owner: Option<String>, ip: Ipv4Addr, ttl: u32) -> Record {
    Record {
        owner,
        ttl: ttl.clamp(1, GUEST_TTL_CAP),
        data: Rdata::A(ip),
    }
}

/// A host record as DNS data if it is of the type asked for and its names are plain; the second
/// value is the name the record points at, if any.
fn convert(rtype: RecordType, record: HostRecord) -> Option<(Rdata, Option<String>)> {
    match (rtype, record) {
        (
            RecordType::Srv,
            HostRecord::Srv {
                priority,
                weight,
                port,
                target,
            },
        ) if names::is_plain(&target) => Some((
            Rdata::Srv {
                priority,
                weight,
                port,
                target: target.clone(),
            },
            Some(target),
        )),
        (RecordType::Txt, HostRecord::Txt { strings }) => Some((Rdata::Txt(strings), None)),
        (
            RecordType::Mx,
            HostRecord::Mx {
                preference,
                exchange,
            },
        ) if names::is_plain(&exchange) => Some((
            Rdata::Mx {
                preference,
                exchange: exchange.clone(),
            },
            Some(exchange),
        )),
        _ => None,
    }
}

/// The stub's sockets: UDP and TCP on the same address. Its tasks end when it is dropped.
#[derive(Debug)]
pub struct DnsServer {
    udp: SocketAddr,
    tcp: SocketAddr,
    tasks: JoinSet<()>,
}

impl DnsServer {
    /// Binds `listen` (UDP and TCP) and starts serving `stub`.
    ///
    /// # Errors
    ///
    /// Binding a socket failing, for example because no interface has the address yet.
    pub async fn bind<A: Ask>(stub: Stub<A>, listen: SocketAddr) -> io::Result<Self> {
        let udp = UdpSocket::bind(listen).await?;
        make_room_for_a_burst(&udp, stub.shared.limits.max_udp_queries);
        let tcp = TcpListener::bind(listen).await?;
        let (udp_addr, tcp_addr) = (udp.local_addr()?, tcp.local_addr()?);
        let limits = stub.shared.limits;
        let mut tasks = JoinSet::new();
        tasks.spawn(serve_udp(udp, stub.clone(), limits));
        tasks.spawn(serve_tcp(tcp, stub, limits));
        Ok(Self {
            udp: udp_addr,
            tcp: tcp_addr,
            tasks,
        })
    }

    /// The UDP address bound.
    #[must_use]
    pub fn udp_addr(&self) -> SocketAddr {
        self.udp
    }

    /// The TCP address bound.
    #[must_use]
    pub fn tcp_addr(&self) -> SocketAddr {
        self.tcp
    }

    /// Runs until a socket task ends (they don't on their own).
    pub async fn run(mut self) {
        while self.tasks.join_next().await.is_some() {}
    }
}

/// What the kernel holds for one waiting query: a small datagram is charged about 700 bytes
/// (its buffers and bookkeeping), not its few dozen bytes of payload.
#[cfg(target_os = "linux")]
const UDP_QUEUE_BYTES_PER_QUERY: usize = 1024;

/// Sizes the socket's receive queue for the queries the stub promises to handle at once. The
/// default queue (about 300 datagrams) is smaller than that promise: a cold package-manager start
/// sends hundreds of queries in a few microseconds, the kernel drops what does not fit, and each
/// dropped query costs its client a whole attempt (five seconds) for a lookup the stub would have
/// answered. The agent is root in the guest, so the size is forced past the `rmem_max` cap when
/// it can be, and capped by it otherwise.
#[cfg(target_os = "linux")]
fn make_room_for_a_burst(socket: &UdpSocket, queries: usize) {
    use rustix::net::sockopt;
    let wanted = queries.max(1).saturating_mul(UDP_QUEUE_BYTES_PER_QUERY);
    if sockopt::set_socket_recv_buffer_size_force(socket, wanted).is_err()
        && let Err(err) = sockopt::set_socket_recv_buffer_size(socket, wanted)
    {
        tracing::debug!(error = %err, "stub DNS: receive queue left at its default size");
    }
}

#[cfg(not(target_os = "linux"))]
fn make_room_for_a_burst(_socket: &UdpSocket, _queries: usize) {}

/// How often [`bind_when_ready`] tries.
const BIND_RETRY: Duration = Duration::from_secs(1);

/// Binds `listen` as soon as an interface has the address, then serves. The address is made by
/// the boot sequence, which may run after the agent starts.
pub async fn bind_when_ready<A: Ask>(stub: Stub<A>, listen: SocketAddr) {
    loop {
        tokio::time::sleep(BIND_RETRY).await;
        match DnsServer::bind(stub.clone(), listen).await {
            Ok(server) => {
                tracing::info!(%listen, "stub DNS listening");
                server.run().await;
                return;
            }
            Err(err) => tracing::debug!(%listen, error = %err, "stub DNS still not bound"),
        }
    }
}

/// Counts what the stub turns away and says so in the log, at most once every ten seconds with
/// the number since the last line: a flood is one line, not one per packet.
struct DropNotice {
    what: &'static str,
    since: Option<Instant>,
    count: u64,
}

impl DropNotice {
    const EVERY: Duration = Duration::from_secs(10);

    fn new(what: &'static str) -> Self {
        Self {
            what,
            since: None,
            count: 0,
        }
    }

    /// Counts one; the number to report when a line is due now.
    fn count_one(&mut self, now: Instant) -> Option<u64> {
        self.count += 1;
        if self
            .since
            .is_some_and(|at| now.duration_since(at) < Self::EVERY)
        {
            return None;
        }
        self.since = Some(now);
        Some(std::mem::take(&mut self.count))
    }

    fn note(&mut self, now: Instant) {
        if let Some(count) = self.count_one(now) {
            tracing::warn!(count, "stub DNS: {}", self.what);
        }
    }
}

async fn serve_udp<A: Ask>(socket: UdpSocket, stub: Stub<A>, limits: Limits) {
    let socket = Arc::new(socket);
    let slots = Arc::new(Semaphore::new(limits.max_udp_queries.max(1)));
    let mut dropped = DropNotice::new("queries dropped: too many at once (the client asks again)");
    let mut tasks = JoinSet::new();
    let mut buf = vec![0u8; MAX_QUERY];
    loop {
        while tasks.try_join_next().is_some() {}
        let (n, peer) = match socket.recv_from(&mut buf).await {
            Ok(received) => received,
            Err(err) => {
                tracing::debug!(error = %err, "udp receive failed");
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
        };
        let Ok(slot) = Arc::clone(&slots).try_acquire_owned() else {
            dropped.note(Instant::now());
            stub.shared.trouble(StubDnsTrouble::UdpDropped);
            continue;
        };
        let message = buf.get(..n).unwrap_or_default().to_vec();
        let (socket, stub) = (Arc::clone(&socket), stub.clone());
        tasks.spawn(async move {
            if let Some(answer) = stub.answer(&message, true).await
                && let Err(err) = socket.send_to(&answer, peer).await
            {
                tracing::debug!(error = %err, "udp answer not sent");
            }
            drop(slot);
        });
    }
}

async fn serve_tcp<A: Ask>(listener: TcpListener, stub: Stub<A>, limits: Limits) {
    let slots = Arc::new(Semaphore::new(limits.max_tcp_connections.max(1)));
    let mut dropped = DropNotice::new("connections closed: too many open at once");
    let mut conns = JoinSet::new();
    loop {
        while conns.try_join_next().is_some() {}
        let (stream, _) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(err) => {
                tracing::debug!(error = %err, "tcp accept failed");
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
        };
        let Ok(slot) = Arc::clone(&slots).try_acquire_owned() else {
            dropped.note(Instant::now());
            stub.shared.trouble(StubDnsTrouble::TcpRefused);
            continue;
        };
        let stub = stub.clone();
        conns.spawn(async move {
            tcp_connection(stream, stub, limits.tcp_idle).await;
            drop(slot);
        });
    }
}

/// Answers length-prefixed queries one after another until the client goes quiet or misbehaves.
async fn tcp_connection<A: Ask>(mut stream: tokio::net::TcpStream, stub: Stub<A>, idle: Duration) {
    loop {
        let mut len = [0u8; 2];
        let read = tokio::time::timeout(idle, stream.read_exact(&mut len)).await;
        if !matches!(read, Ok(Ok(_))) {
            return;
        }
        let len = usize::from(u16::from_be_bytes(len));
        if len == 0 || len > MAX_QUERY {
            return;
        }
        let mut message = vec![0u8; len];
        if !matches!(
            tokio::time::timeout(idle, stream.read_exact(&mut message)).await,
            Ok(Ok(_))
        ) {
            return;
        }
        let Some(answer) = stub.answer(&message, false).await else {
            return;
        };
        let mut framed = u16::try_from(answer.len())
            .unwrap_or(u16::MAX)
            .to_be_bytes()
            .to_vec();
        framed.extend_from_slice(&answer);
        if stream.write_all(&framed).await.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests;
