// SPDX-License-Identifier: AGPL-3.0-or-later
use futures_util::FutureExt as _;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::Poll;
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

/// One poll of `fut`, without waiting for it.
async fn poll_once<F: Future + Unpin>(fut: &mut F) -> Poll<F::Output> {
    std::future::poll_fn(|cx| Poll::Ready(Pin::new(&mut *fut).poll(cx))).await
}

#[tokio::test(start_paused = true)]
async fn a_connect_after_the_answer_looks_again_even_while_an_old_waiter_is_still_there() {
    let r = Coalescing::new(Slow::new(Duration::from_secs(5)));
    let n = name("a.example");
    let mut first = Box::pin(r.resolve(&n, 443));
    let mut second = Box::pin(r.resolve(&n, 443));
    assert!(poll_once(&mut first).await.is_pending());
    assert!(poll_once(&mut second).await.is_pending());
    tokio::time::advance(Duration::from_secs(6)).await;
    // The first caller ran the lookup; the second has not been polled since and still waits.
    let answer = (&mut first).now_or_never().expect("the lookup is over");
    assert_eq!(ip(&answer.unwrap()), 1);

    let mut later = Box::pin(r.resolve(&n, 443));
    assert!(poll_once(&mut later).await.is_pending(), "a new lookup");
    tokio::time::advance(Duration::from_secs(6)).await;
    let answer = (&mut later).now_or_never().expect("the new lookup is over");
    assert_eq!(ip(&answer.unwrap()), 2);
    assert_eq!(r.inner.lookups.load(Ordering::SeqCst), 2);

    let answer = (&mut second)
        .now_or_never()
        .expect("the old waiter gets the old answer");
    assert_eq!(ip(&answer.unwrap()), 1);
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
                    let _ =
                        tokio::time::timeout(Duration::from_millis(1), r.resolve(&n, 443)).await;
                })
            })
            .collect();
        for t in tasks {
            t.await.unwrap();
        }
        assert!(r.flights.lock().unwrap().is_empty());
    }
}
