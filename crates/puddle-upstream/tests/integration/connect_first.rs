// SPDX-License-Identifier: AGPL-3.0-or-later
//! Connecting to a destination with several addresses over real sockets. The timing of a race
//! against an address that never answers is a unit test with a stand-in connector in `chain`:
//! what the kernel does with a full backlog differs between systems, so no real socket can be a
//! deterministic black hole.

use puddle_upstream::connect_first;
use tokio::net::TcpListener;

#[tokio::test]
async fn an_address_that_refuses_hands_over_to_the_next_one() {
    // A port nothing listens on: bound, then released.
    let refused = TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap()
        .local_addr()
        .unwrap();
    let good = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let good_addr = good.local_addr().unwrap();
    let (_, addr) = connect_first(&[refused, good_addr], std::time::Duration::from_secs(10))
        .await
        .unwrap();
    assert_eq!(addr, good_addr);
}
