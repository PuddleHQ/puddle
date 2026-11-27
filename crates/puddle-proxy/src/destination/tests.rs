// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
#[cfg(windows)]
use windows_sys::Win32::Networking::WinSock::{
    WSA_NOT_ENOUGH_MEMORY, WSAENETDOWN, WSAHOST_NOT_FOUND, WSANO_DATA, WSANO_RECOVERY, WSATRY_AGAIN,
};

#[test]
fn a_name_that_does_not_exist_is_told_apart_from_a_lookup_that_failed() {
    #[cfg(unix)]
    let codes = [
        ("EAI_NONAME", libc::EAI_NONAME, true),
        ("EAI_NODATA", libc::EAI_NODATA, true),
        ("EAI_AGAIN", libc::EAI_AGAIN, false),
        ("EAI_FAIL", libc::EAI_FAIL, false),
        ("EAI_MEMORY", libc::EAI_MEMORY, false),
        ("EAI_SYSTEM", libc::EAI_SYSTEM, false),
        ("an unknown code", -9999, false),
    ];
    #[cfg(windows)]
    let codes = [
        ("WSAHOST_NOT_FOUND", WSAHOST_NOT_FOUND, true),
        ("WSANO_DATA", WSANO_DATA, true),
        ("WSATRY_AGAIN", WSATRY_AGAIN, false),
        ("WSANO_RECOVERY", WSANO_RECOVERY, false),
        ("WSA_NOT_ENOUGH_MEMORY", WSA_NOT_ENOUGH_MEMORY, false),
        ("WSAENETDOWN", WSAENETDOWN, false),
    ];
    for (label, code, missing) in codes {
        let err = name_error(LookupError::new(code));
        assert_eq!(err.kind() == io::ErrorKind::NotFound, missing, "{label}");
        // The system's own text stays, whichever way it was classified.
        assert!(!err.to_string().is_empty(), "{label}");
    }
}

#[tokio::test]
async fn a_name_under_invalid_is_no_such_name_without_a_lookup() {
    for host in [
        "invalid",
        "site.invalid",
        "a.b.invalid",
        "SITE.Invalid",
        "site.invalid.",
    ] {
        let Ok(name) = DomainName::parse_normalised(host) else {
            // A form the type refuses never reaches the resolver; the helper still decides it.
            assert!(is_invalid_name(host), "{host}");
            continue;
        };
        let err = SystemResolver.resolve(&name, 443).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound, "{host}");
        assert!(err.to_string().contains(".invalid"), "{host}: {err}");
    }
}

#[test]
fn only_the_last_label_makes_a_name_invalid() {
    for host in [
        "notinvalid.com",
        "invalid.example.com",
        "invalid.com",
        "my-invalid",
        "xinvalid",
        "invalid.test",
        "localhost",
        "",
    ] {
        assert!(!is_invalid_name(host), "{host}");
    }
}

#[tokio::test]
async fn the_system_resolver_resolves_localhost() {
    let name = DomainName::parse_normalised("localhost").unwrap();
    let addrs = SystemResolver.resolve(&name, 8080).await.unwrap();
    assert_ne!(addrs.len(), 0);
    assert!(
        addrs
            .iter()
            .all(|a| a.port() == 8080 && a.ip().is_loopback())
    );
}

/// A double that blocks every address with the reason it was built with, or allows all.
struct Fixed(Option<BlockReason>);

impl AddressCheck for Fixed {
    fn check(&self, _: &WorkspaceId, _: SocketAddr) -> AddressVerdict {
        self.0.map_or(AddressVerdict::Allow, AddressVerdict::Block)
    }
}

#[test]
fn the_default_name_stage_blocks_an_ip_literal_the_address_check_blocks() {
    let workspace = WorkspaceId::new("box").unwrap();
    let literal = puddle_netpolicy::normalise_host("203.0.113.9").unwrap();
    let name = puddle_netpolicy::normalise_host("example.com").unwrap();
    let blocking = Fixed(Some(BlockReason::PuddleEndpoint));
    assert_eq!(
        blocking.check_target(&workspace, &literal, 443),
        Some(BlockReason::PuddleEndpoint)
    );
    assert_eq!(blocking.check_target(&workspace, &name, 443), None);
    assert_eq!(Fixed(None).check_target(&workspace, &literal, 443), None);
}

#[test]
fn a_numeric_lookup_gives_one_stream_address_with_the_port() {
    let addrs = lookup("127.0.0.1", 8080).unwrap();
    assert_eq!(addrs, ["127.0.0.1:8080".parse::<SocketAddr>().unwrap()]);
}

#[test]
fn a_double_lets_no_distribution_point_through_unless_it_says_so() {
    let host = Host::parse_normalised("pki.corp.example").unwrap();
    let addr: SocketAddr = "10.1.2.3:80".parse().unwrap();
    assert!(!Fixed(None).pki_address(&host, addr));
}

#[test]
fn the_product_guard_lets_a_distribution_point_use_private_addresses_only() {
    let guard = NetPolicy::new(Arc::new(puddle_netpolicy::LocalAccess::NONE));
    let host = Host::parse_normalised("pki.corp.example").unwrap();
    let check: &dyn AddressCheck = &guard;
    for (addr, expected) in [
        ("10.1.2.3:80", true),
        ("192.168.0.9:80", true),
        ("127.0.0.1:80", false),
        ("169.254.169.254:80", false),
        ("8.8.8.8:80", false),
    ] {
        assert_eq!(
            check.pki_address(&host, addr.parse().unwrap()),
            expected,
            "{addr}"
        );
    }
}

/// Jobs run on the slots' blocking threads at most `limit` at a time, however many are queued,
/// and a job that is queued starts once a slot frees (a lookup's timeout drops its waiter, never
/// its thread, so only the slots bound the threads).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn blocking_jobs_run_at_most_as_many_at_a_time_as_there_are_slots() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    let slots = Arc::new(BlockingSlots::new(2));
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let release = Arc::new(AtomicBool::new(false));
    let jobs: Vec<_> = (0..6)
        .map(|n| {
            let (slots, active, peak, release) = (
                Arc::clone(&slots),
                Arc::clone(&active),
                Arc::clone(&peak),
                Arc::clone(&release),
            );
            tokio::spawn(async move {
                slots
                    .run(move || {
                        let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(now, Ordering::SeqCst);
                        let deadline = Instant::now() + Duration::from_secs(10);
                        while !release.load(Ordering::SeqCst) && Instant::now() < deadline {
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        active.fetch_sub(1, Ordering::SeqCst);
                        n
                    })
                    .await
            })
        })
        .collect();
    let deadline = Instant::now() + Duration::from_secs(5);
    while active.load(Ordering::SeqCst) < 2 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(active.load(Ordering::SeqCst), 2, "two run, four wait");
    release.store(true, Ordering::SeqCst);
    for (n, job) in jobs.into_iter().enumerate() {
        assert_eq!(job.await.unwrap().unwrap(), n);
    }
    assert_eq!(peak.load(Ordering::SeqCst), 2);
}
