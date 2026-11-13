// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

/// A vsock socket, or `None` where the kernel has no `AF_VSOCK` (the test then has nothing to
/// check). The Linux CI job sets `PUDDLE_REQUIRE_VSOCK`, which makes a missing `AF_VSOCK` a failure.
fn vsock() -> Option<OwnedFd> {
    match socket() {
        Ok(fd) => Some(fd),
        Err(err) => {
            assert!(
                std::env::var_os("PUDDLE_REQUIRE_VSOCK").is_none(),
                "PUDDLE_REQUIRE_VSOCK is set but this kernel has no AF_VSOCK ({err})"
            );
            eprintln!("skipped: no AF_VSOCK here ({err})");
            None
        }
    }
}

#[test]
fn the_buffer_is_raised_past_the_hosts_window() {
    let Some(fd) = vsock() else { return };
    let got = set_buffer(fd.as_fd(), 16 << 20).unwrap();
    // 16 MiB > msb's 8 MiB, so the send window becomes the host's and its 4 MiB updates arrive.
    assert_eq!(got, (16 << 20, 16 << 20));
}

#[test]
fn the_kernel_default_is_256_kib_without_the_fix() {
    let Some(fd) = vsock() else { return };
    assert_eq!(
        get_u64(fd.as_fd(), SO_VM_SOCKETS_BUFFER_SIZE).unwrap(),
        256 * 1024
    );
}

#[test]
fn a_bad_option_is_an_error_not_a_panic() {
    let Some(fd) = vsock() else { return };
    assert!(set_u64(fd.as_fd(), 9999, 1).is_err());
    assert!(get_u64(fd.as_fd(), 9999).is_err());
}

#[test]
fn connecting_to_nobody_fails_cleanly() {
    if vsock().is_none() {
        return;
    }
    // CID 1 is the local loopback; nothing listens on this port.
    assert!(connect(1, 0x7fff_fff0, 16 << 20).is_err());
    assert!(connect(1, 0x7fff_fff0, 0).is_err());
}
