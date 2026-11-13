// SPDX-License-Identifier: GPL-3.0-or-later
use std::future::{pending, ready};

use super::*;
use crate::hop::Scheme;

const WITHIN: Duration = Duration::from_millis(100);

fn dest() -> Destination {
    Destination::new(Scheme::Https, "api.git-host.test", 443)
}

#[tokio::test]
async fn a_lookup_that_never_answers_gives_no_addresses_at_its_bound() {
    let got = resolve(WITHIN, &dest(), |_, _| {
        pending::<io::Result<std::vec::IntoIter<SocketAddr>>>()
    })
    .await;
    assert_eq!(got, Vec::<SocketAddr>::new());
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

#[test]
fn a_stand_in_lookup_prints_without_showing_its_closure() {
    let lookup = crate::chain::HostLookup::new(|_, _| async { Ok(Vec::new()) });
    assert_eq!(format!("{lookup:?}"), "HostLookup(..)");
}
