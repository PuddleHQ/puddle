// SPDX-License-Identifier: GPL-3.0-or-later
//! One lookup in flight per name: connects that ask for a name while a lookup of it is running
//! share that lookup's answer instead of each starting their own.
//!
//! It is not a cache. The answer is handed to the callers that were waiting when it arrived and
//! forgotten, so no answer outlives the lookup it came from and a name that changes is seen by the
//! next connect. (The OS resolver reports no TTL, so a cache would have to guess one.) Rules are
//! decided before a name is resolved and the address check runs on every connect after it, so
//! sharing an answer between connects changes neither.

use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};

use puddle_types::DomainName;
use tokio::sync::OnceCell;

use crate::destination::{BoxFuture, Resolver};

/// The lookup's outcome as every waiter gets it: the error's kind and text, not its source chain.
type Answer = Result<Vec<SocketAddr>, (io::ErrorKind, String)>;
type Key = (DomainName, u16);

/// A lookup in flight and how many callers wait on it.
struct Flight {
    cell: Arc<OnceCell<Answer>>,
    waiters: usize,
}

type Flights = Mutex<HashMap<Key, Flight>>;

/// `inner`, with concurrent lookups of the same name and port shared. An answer is kept only
/// until its lookup returns; a user-supplied resolver is not wrapped.
pub struct Coalescing<R> {
    inner: R,
    flights: Flights,
}

impl<R: Resolver> Coalescing<R> {
    /// Shares `inner`'s concurrent lookups.
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            flights: Mutex::new(HashMap::new()),
        }
    }
}

fn lock(flights: &Flights) -> std::sync::MutexGuard<'_, HashMap<Key, Flight>> {
    flights.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Removes `key`'s flight if it is still `cell`'s (a later caller may have started a new one).
fn remove_if_ours(map: &mut HashMap<Key, Flight>, key: &Key, cell: &Arc<OnceCell<Answer>>) {
    if map.get(key).is_some_and(|f| Arc::ptr_eq(&f.cell, cell)) {
        map.remove(key);
    }
}

/// One waiter on a flight. The last waiter to leave takes the flight out of the map, so the map
/// never keeps a name whose every caller gave up (the connect's resolve timeout drops them).
struct Waiter<'a> {
    flights: &'a Flights,
    key: Key,
    cell: Arc<OnceCell<Answer>>,
}

impl<'a> Waiter<'a> {
    fn join(flights: &'a Flights, key: Key) -> Self {
        let mut map = lock(flights);
        let flight = map.entry(key.clone()).or_insert_with(|| Flight {
            cell: Arc::default(),
            waiters: 0,
        });
        flight.waiters += 1;
        let cell = Arc::clone(&flight.cell);
        drop(map);
        Self { flights, key, cell }
    }
}

impl Drop for Waiter<'_> {
    fn drop(&mut self) {
        let mut map = lock(self.flights);
        if let Some(flight) = map.get_mut(&self.key)
            && Arc::ptr_eq(&flight.cell, &self.cell)
        {
            flight.waiters -= 1;
            if flight.waiters == 0 {
                map.remove(&self.key);
            }
        }
    }
}

impl<R: Resolver> Resolver for Coalescing<R> {
    fn resolve<'a>(
        &'a self,
        name: &'a DomainName,
        port: u16,
    ) -> BoxFuture<'a, io::Result<Vec<SocketAddr>>> {
        Box::pin(async move {
            let waiter = Waiter::join(&self.flights, (name.clone(), port));
            let answer = waiter
                .cell
                .get_or_init(|| async {
                    let answer = self
                        .inner
                        .resolve(name, port)
                        .await
                        .map_err(|e| (e.kind(), e.to_string()));
                    // Out of the map the moment it has an answer, before the waiters wake, so a
                    // connect that comes after looks again.
                    remove_if_ours(&mut lock(&self.flights), &waiter.key, &waiter.cell);
                    answer
                })
                .await;
            match answer {
                Ok(addrs) => Ok(addrs.clone()),
                Err((kind, text)) => Err(io::Error::new(*kind, text.clone())),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use super::*;

    /// Answers after `delay` (the paused clock), counting its lookups; the answer is the lookup's
    /// number, as the port-1 address's last octet, so a stale answer shows.
    struct Slow {
        lookups: AtomicUsize,
        delay: Duration,
        fail: bool,
    }

    impl Slow {
        fn new(delay: Duration) -> Self {
            Self {
                lookups: AtomicUsize::new(0),
                delay,
                fail: false,
            }
        }
    }

    impl Resolver for Slow {
        fn resolve<'a>(
            &'a self,
            _name: &'a DomainName,
            port: u16,
        ) -> BoxFuture<'a, io::Result<Vec<SocketAddr>>> {
            Box::pin(async move {
                let n = self.lookups.fetch_add(1, Ordering::SeqCst) + 1;
                // Simulated latency of the system resolver.
                tokio::time::sleep(self.delay).await;
                if self.fail {
                    return Err(io::Error::new(io::ErrorKind::NotFound, "gone"));
                }
                Ok(vec![SocketAddr::from((
                    [10, 0, 0, u8::try_from(n).unwrap()],
                    port,
                ))])
            })
        }
    }

    fn name(s: &str) -> DomainName {
        DomainName::parse_normalised(s).unwrap()
    }

    fn ip(addrs: &[SocketAddr]) -> u8 {
        let text = addrs[0].ip().to_string();
        text.rsplit('.').next().unwrap().parse().unwrap()
    }

    #[tokio::test(start_paused = true)]
    async fn concurrent_connects_to_one_name_share_one_lookup() {
        let r = Coalescing::new(Slow::new(Duration::from_secs(5)));
        let n = name("a.example");
        let all = futures_util::future::join_all((0..50).map(|_| r.resolve(&n, 443))).await;
        assert_eq!(r.inner.lookups.load(Ordering::SeqCst), 1);
        for answer in all {
            assert_eq!(ip(&answer.unwrap()), 1);
        }
        assert!(r.flights.lock().unwrap().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn different_names_and_ports_are_looked_up_apart() {
        let r = Coalescing::new(Slow::new(Duration::from_secs(1)));
        let (a, b) = (name("a.example"), name("b.example"));
        let _ = tokio::join!(r.resolve(&a, 443), r.resolve(&b, 443), r.resolve(&a, 80));
        assert_eq!(r.inner.lookups.load(Ordering::SeqCst), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn no_answer_is_kept_past_its_lookup() {
        let r = Coalescing::new(Slow::new(Duration::from_secs(1)));
        let n = name("a.example");
        assert_eq!(ip(&r.resolve(&n, 443).await.unwrap()), 1);
        // The clock moves a long way and nothing is kept: the next connect looks again.
        tokio::time::advance(Duration::from_secs(3600)).await;
        assert_eq!(ip(&r.resolve(&n, 443).await.unwrap()), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn a_connect_arriving_mid_lookup_shares_it_and_one_after_does_not() {
        let r = Arc::new(Coalescing::new(Slow::new(Duration::from_secs(5))));
        let n = name("a.example");
        let first = tokio::spawn({
            let (r, n) = (r.clone(), n.clone());
            async move { r.resolve(&n, 443).await }
        });
        tokio::time::advance(Duration::from_secs(3)).await;
        let late = ip(&r.resolve(&n, 443).await.unwrap());
        assert_eq!(late, 1, "joined the lookup in flight");
        assert_eq!(ip(&first.await.unwrap().unwrap()), 1);
        assert_eq!(ip(&r.resolve(&n, 443).await.unwrap()), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn a_failure_is_shared_with_its_kind_and_not_kept() {
        let mut slow = Slow::new(Duration::from_secs(1));
        slow.fail = true;
        let r = Coalescing::new(slow);
        let n = name("a.example");
        let all = futures_util::future::join_all((0..5).map(|_| r.resolve(&n, 443))).await;
        assert_eq!(r.inner.lookups.load(Ordering::SeqCst), 1);
        for answer in all {
            assert_eq!(answer.unwrap_err().kind(), io::ErrorKind::NotFound);
        }
        let _ = r.resolve(&n, 443).await;
        assert_eq!(r.inner.lookups.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn a_caller_that_times_out_leaves_the_others_their_answer_and_the_map_clean() {
        let r = Arc::new(Coalescing::new(Slow::new(Duration::from_secs(5))));
        let n = name("a.example");
        let patient = tokio::spawn({
            let (r, n) = (r.clone(), n.clone());
            async move { r.resolve(&n, 443).await }
        });
        // The caller that started the lookup gives up (the connect's resolve timeout).
        let impatient = tokio::time::timeout(Duration::from_secs(1), r.resolve(&n, 443)).await;
        assert!(impatient.is_err());
        // A third connect, mid-lookup, joins the patient caller's lookup (which took the
        // abandoned one over) instead of starting a fourth.
        tokio::time::advance(Duration::from_secs(2)).await;
        let third = ip(&r.resolve(&n, 443).await.unwrap());
        assert_eq!(r.inner.lookups.load(Ordering::SeqCst), 2);
        assert_eq!(ip(&patient.await.unwrap().unwrap()), third);
        assert!(r.flights.lock().unwrap().is_empty());
        // Everyone gave up: nothing is left behind either.
        let _ = tokio::time::timeout(Duration::from_secs(1), r.resolve(&n, 443)).await;
        assert!(r.flights.lock().unwrap().is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn many_callers_cancelled_at_once_leave_the_map_empty() {
        let r = Arc::new(Coalescing::new(Slow::new(Duration::from_secs(60))));
        let n = name("a.example");
        for _ in 0..200 {
            let tasks: Vec<_> = (0..16)
                .map(|_| {
                    let (r, n) = (r.clone(), n.clone());
                    tokio::spawn(async move {
                        let _ = tokio::time::timeout(Duration::from_millis(1), r.resolve(&n, 443))
                            .await;
                    })
                })
                .collect();
            for t in tasks {
                t.await.unwrap();
            }
            assert!(r.flights.lock().unwrap().is_empty());
        }
    }
}
