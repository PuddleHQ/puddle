// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn the_families_alternate_from_the_first_address_on_each_in_its_own_order() {
    let a: Vec<SocketAddr> = ["[::1]:1", "[::2]:1", "[::3]:1", "10.0.0.1:1", "10.0.0.2:1"]
        .iter()
        .map(|s| s.parse().unwrap())
        .collect();
    let order: Vec<String> = interleaved(&a).iter().map(ToString::to_string).collect();
    assert_eq!(
        order,
        ["[::1]:1", "10.0.0.1:1", "[::2]:1", "10.0.0.2:1", "[::3]:1"]
    );
    let b: Vec<SocketAddr> = ["10.0.0.1:1", "10.0.0.2:1", "10.0.0.3:1", "[::1]:1"]
        .iter()
        .map(|s| s.parse().unwrap())
        .collect();
    let order: Vec<String> = interleaved(&b).iter().map(ToString::to_string).collect();
    assert_eq!(order, ["10.0.0.1:1", "[::1]:1", "10.0.0.2:1", "10.0.0.3:1"]);
    assert_eq!(interleaved(&[]), Vec::<SocketAddr>::new());
}

/// The error a connect gets when the system cannot give it a local port or buffer.
fn no_buffers() -> io::Error {
    #[cfg(windows)]
    return io::Error::from_raw_os_error(10055);
    #[cfg(target_os = "linux")]
    return io::Error::from_raw_os_error(105);
    #[cfg(not(any(windows, target_os = "linux")))]
    return io::Error::from(io::ErrorKind::AddrNotAvailable);
}

#[tokio::test(start_paused = true)]
async fn a_connect_that_found_no_local_port_tries_again_and_gets_through() {
    let tries = std::cell::Cell::new(0);
    let got = connect_through_port_pressure(|| {
        tries.set(tries.get() + 1);
        let fail = tries.get() <= 3;
        async move {
            if fail {
                Err(no_buffers())
            } else {
                Ok("connected")
            }
        }
    })
    .await;
    assert_eq!(got.unwrap(), "connected");
    assert_eq!(tries.get(), 4);
}

#[tokio::test(start_paused = true)]
async fn a_connect_that_never_finds_a_local_port_gives_up_with_that_error() {
    let tries = std::cell::Cell::new(0);
    let got: io::Result<()> = connect_through_port_pressure(|| {
        tries.set(tries.get() + 1);
        async { Err(no_buffers()) }
    })
    .await;
    assert!(is_port_pressure(&got.unwrap_err()));
    assert_eq!(tries.get(), PORT_RETRIES + 1);
}

#[tokio::test(start_paused = true)]
async fn a_refused_connect_is_not_tried_again() {
    let tries = std::cell::Cell::new(0);
    let got: io::Result<()> = connect_through_port_pressure(|| {
        tries.set(tries.get() + 1);
        async { Err(io::Error::from(io::ErrorKind::ConnectionRefused)) }
    })
    .await;
    assert_eq!(got.unwrap_err().kind(), io::ErrorKind::ConnectionRefused);
    assert_eq!(tries.get(), 1);
}
