// SPDX-License-Identifier: GPL-3.0-or-later
//! Connections Puddle makes for itself: telemetry upload, downloads, anything the
//! host program fetches. They take the same route and authentication as a workspace's traffic.
//!
//! Unlike a guest's request there is nothing to guard: the destination is a fixed endpoint of
//! Puddle's own choosing, so every resolved address counts as checked. A name that cannot be
//! resolved here is left to the company's proxy, as for a guest. TLS goes on top of the returned
//! stream with [`crate::tls_connect`] (the platform verifier plus the corporate roots).

use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::time::Duration;

use crate::chain::{Chain, ChainError, Connected, Form, Request};
use crate::hop::Destination;

/// Connects Puddle itself to `destination` over `chain`.
///
/// # Errors
/// [`ChainError`], as for [`Chain::connect`].
pub async fn connect(
    chain: &Chain,
    destination: &Destination,
    form: Form,
) -> Result<Connected, ChainError> {
    let within = chain.lookup_timeout();
    let resolved = match chain.host_lookup() {
        Some(stand_in) => {
            resolve(within, destination, |host, port| async move {
                stand_in.call(&host, port).await.map(Vec::into_iter)
            })
            .await
        }
        None => {
            resolve(within, destination, |host, port| {
                tokio::net::lookup_host((host, port))
            })
            .await
        }
    };
    let request = Request::new(destination, form, &resolved).name_ok(true);
    chain.connect(&request).await
}

/// The addresses `lookup` gives for `destination`, or none when it fails or takes longer than
/// `within`. The system resolver has no bound of its own: a resolver that is slow or unreachable
/// (a split-DNS name, a VPN that is down) holds the call for as long as its retries last, and the
/// proxy that can resolve the name is only asked afterwards.
async fn resolve<I, F>(
    within: Duration,
    destination: &Destination,
    lookup: impl FnOnce(String, u16) -> F,
) -> Vec<SocketAddr>
where
    I: Iterator<Item = SocketAddr>,
    F: Future<Output = io::Result<I>>,
{
    let host = destination.host();
    match tokio::time::timeout(within, lookup(host.to_owned(), destination.port())).await {
        Ok(Ok(addrs)) => addrs.collect(),
        Ok(Err(err)) => {
            tracing::debug!(host, error = %err, "host-side lookup failed; the proxy may resolve it");
            Vec::new()
        }
        Err(_) => {
            tracing::debug!(
                host,
                ?within,
                "host-side lookup timed out; the proxy may resolve it"
            );
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::future::{pending, ready};
    use std::time::Instant;

    use super::*;
    use crate::hop::Scheme;

    const WITHIN: Duration = Duration::from_millis(100);

    fn dest() -> Destination {
        Destination::new(Scheme::Https, "api.git-host.test", 443)
    }

    #[tokio::test]
    async fn a_lookup_that_never_answers_gives_no_addresses_at_its_bound() {
        let started = Instant::now();
        let got = resolve(WITHIN, &dest(), |_, _| {
            pending::<io::Result<std::vec::IntoIter<SocketAddr>>>()
        })
        .await;
        assert_eq!(got, Vec::<SocketAddr>::new());
        // The bound, not the resolver's own retries, decides how long the connection waits.
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn a_lookup_that_fails_gives_no_addresses() {
        let got = resolve(WITHIN, &dest(), |_, _| {
            ready(io::Result::<std::vec::IntoIter<SocketAddr>>::Err(
                io::ErrorKind::NotFound.into(),
            ))
        })
        .await;
        assert_eq!(got, Vec::<SocketAddr>::new());
    }

    #[tokio::test]
    async fn a_lookup_that_answers_gives_its_addresses_for_the_name_and_port_asked() {
        let ip = SocketAddr::from(([192, 0, 2, 7], 443));
        let got = resolve(WITHIN, &dest(), |host, port| {
            assert_eq!((host.as_str(), port), ("api.git-host.test", 443));
            ready(Ok(vec![ip].into_iter()))
        })
        .await;
        assert_eq!(got, vec![ip]);
    }
}
