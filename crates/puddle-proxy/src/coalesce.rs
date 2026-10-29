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

type Answer = Result<Vec<SocketAddr>, (io::ErrorKind, String)>;
type Key = (DomainName, u16);
type Flights = Mutex<HashMap<Key, Arc<OnceCell<Answer>>>>;

/// `inner`, with concurrent lookups of the same name and port shared.
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

/// Takes the flight out of the map when its caller is done, so the map never keeps a name whose
/// lookup finished or whose every caller gave up (the connect's resolve timeout drops it).
struct Leave<'a> {
    flights: &'a Flights,
    key: Key,
    cell: Arc<OnceCell<Answer>>,
}

impl Drop for Leave<'_> {
    fn drop(&mut self) {
        let mut flights = self.flights.lock().unwrap_or_else(PoisonError::into_inner);
        let ours = flights
            .get(&self.key)
            .is_some_and(|c| Arc::ptr_eq(c, &self.cell));
        // The map and this guard hold it, nobody else is waiting; or it has an answer, so a later
        // caller must look again.
        if ours && (self.cell.initialized() || Arc::strong_count(&self.cell) == 2) {
            flights.remove(&self.key);
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
            let key = (name.clone(), port);
            let cell = self
                .flights
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(key.clone())
                .or_default()
                .clone();
            let leave = Leave {
                flights: &self.flights,
                key,
                cell,
            };
            let cell = &leave.cell;
            let answer = cell
                .get_or_init(|| async {
                    // Out of the map the moment it has an answer, before the waiters wake.
                    let answer = self
                        .inner
                        .resolve(name, port)
                        .await
                        .map_err(|e| (e.kind(), e.to_string()));
                    let mut flights = self.flights.lock().unwrap_or_else(PoisonError::into_inner);
                    if flights
                        .get(&(name.clone(), port))
                        .is_some_and(|c| Arc::ptr_eq(c, cell))
                    {
                        flights.remove(&(name.clone(), port));
                    }
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
        match addrs[0].ip() {
            std::net::IpAddr::V4(v4) => v4.octets()[3],
            std::net::IpAddr::V6(_) => unreachable!(),
        }
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
        // The clock moves, nothing is cached: the next connect looks again and sees the new answer.
        tokio::time::advance(Duration::from_millis(1)).await;
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
        assert!(patient.await.unwrap().is_ok());
        assert!(r.flights.lock().unwrap().is_empty());
        // Everyone gave up: nothing is left behind either.
        let _ = tokio::time::timeout(Duration::from_secs(1), r.resolve(&n, 443)).await;
        assert!(r.flights.lock().unwrap().is_empty());
    }
}
