// SPDX-License-Identifier: GPL-3.0-or-later
//! Proof that the stall meter of the transparency bar (`support::stall`) leaves out the kernel's
//! retransmission backoff and nothing else. The bar is "no gap over 5 s that the transfer's own
//! parties caused"; it holds only if a pause by a reader still fails it and a dropped segment's
//! backoff does not. Both are planted here on real loopback sockets, with a shorter bound so the
//! cases run in seconds:
//!
//! - a reader that stops reading for 3 s leaves the sender on the zero-window probe timer, which
//!   the meter must not forgive;
//! - a network namespace whose loopback drops every packet for 4 s makes the kernel back off
//!   (timeout retransmissions at doubling intervals), which the meter must subtract. The namespace is
//!   made by `unshare -rn`, or `sudo -n unshare -n` where unprivileged namespaces are off.
#![cfg(target_os = "linux")]
#![expect(clippy::unwrap_used, reason = "a test fails by panicking")]

use crate::support;

use std::process::Command;
use std::time::{Duration, Instant};

use support::stall::{Accrual, Backoff, Progress, parse};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// The bound the planted cases use: the pause is over it on its own, the drop's raw gap is over
/// it by half again, and what the meter leaves of that gap is under it. What it leaves is the
/// kernel's own crawl after a recovery (0.03 to 1.7 s in the runs here), which has no signal in
/// `/proc/net/tcp` to subtract.
const BOUND: Duration = Duration::from_secs(3);
const PAUSE: Duration = Duration::from_secs(4);
/// How long the namespace's loopback drops everything: long enough for the backoff to double
/// past the bound twice.
const DROP_FOR: Duration = Duration::from_secs(4);
const CHUNK: usize = 1 << 20;
const INNER: &str = "PUDDLE_STALL_METER_INNER";

const HEAD: &str =
    "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid\n";

/// One `/proc/net/tcp` row of a loopback connection `local` to `remote`.
fn row(local: u16, remote: u16, timer: u8, when: u64, retransmits: u32) -> String {
    format!(
        "   0: 0100007F:{local:04X} 0100007F:{remote:04X} 01 00000000:00000000 {timer:02X}:{when:08X} {retransmits:08X} 1000 0 1 1 0 0 0\n"
    )
}

/// Feeds one sample per 20 ms, from `t0`, of a socket on 40000 to 5201 and returns the time
/// accrued: each step is `(timer, when, retransmits)`.
fn accrued(steps: &[(u8, u64, u32)]) -> Duration {
    let t0 = Instant::now();
    let mut accrual = Accrual::default();
    for (i, &(timer, when, retransmits)) in steps.iter().enumerate() {
        let table = format!("{HEAD}{}", row(40000, 5201, timer, when, retransmits));
        let now = t0 + Duration::from_millis(20 * i as u64);
        accrual.observe(now, &parse(&table, &[5201]));
    }
    accrual.total()
}

#[test]
fn a_timeout_retransmission_counts_from_the_last_ack_that_armed_the_timer() {
    // Armed with 20 jiffies at sample 1 (the last ACK), counting down 18, 16 (samples 2, 3), the
    // timeout fires and retransmits=1 shows at sample 4, still in backoff at sample 5.
    let steps = [
        (1, 5, 0),
        (1, 20, 0),
        (1, 18, 0),
        (1, 16, 0),
        (1, 40, 1),
        (1, 38, 1),
    ];
    // From sample 1 (t=20 ms) to sample 5 (t=100 ms).
    assert_eq!(accrued(&steps), Duration::from_millis(80));
}

#[test]
fn a_timer_that_keeps_being_re_armed_is_not_backoff() {
    // Data flows: every ACK re-arms the timer, nothing was retransmitted.
    let steps = [(1, 20, 0), (1, 19, 0), (1, 20, 0), (1, 19, 0), (1, 20, 0)];
    assert_eq!(accrued(&steps), Duration::ZERO);
}

#[test]
fn a_closed_window_is_not_backoff() {
    // The zero-window probe timer (tr 4) is what a stopped reader leaves the sender on, for as
    // long as the reader waits, and backing off its probes does not make it kernel time.
    let steps = [(4, 20, 0), (4, 18, 0), (4, 80, 0), (4, 78, 0), (4, 160, 0)];
    assert_eq!(accrued(&steps), Duration::ZERO);
    // Even a probe timer that carried a retransmit count is not the retransmit timer.
    let steps = [(4, 20, 3), (4, 18, 3), (4, 80, 3), (4, 78, 3)];
    assert_eq!(accrued(&steps), Duration::ZERO);
}

#[test]
fn a_tail_loss_probe_timer_is_not_backoff() {
    let steps = [(5, 20, 0), (5, 18, 0), (5, 16, 1), (5, 14, 1)];
    assert_eq!(accrued(&steps), Duration::ZERO);
}

#[test]
fn a_backoff_that_ends_stops_counting() {
    let steps = [
        (1, 20, 0),
        (1, 18, 0),
        (1, 40, 1),
        (1, 38, 1),
        (1, 20, 0),
        (1, 18, 0),
    ];
    // The countdown seen from sample 0 (0 ms) to sample 3 (60 ms); the ACK at sample 4 ends it.
    assert_eq!(accrued(&steps), Duration::from_millis(60));
}

#[test]
fn another_connection_in_backoff_is_not_ours() {
    let table = format!("{HEAD}{}", row(41000, 6000, 1, 40, 4));
    assert!(parse(&table, &[5201, 40000]).is_empty());
}

#[test]
fn two_sockets_in_backoff_at_once_count_once() {
    let t0 = Instant::now();
    let mut accrual = Accrual::default();
    for i in 0..4u64 {
        let when = 40 - 2 * i;
        let table = format!(
            "{HEAD}{}{}",
            row(40000, 5201, 1, when, 1),
            row(5201, 40000, 1, when, 1)
        );
        accrual.observe(t0 + Duration::from_millis(20 * i), &parse(&table, &[5201]));
    }
    assert_eq!(accrual.total(), Duration::from_millis(60));
}

/// Writes 1 MiB chunks to `server` for `run` and returns the progress it measured.
async fn send_for<'a>(client: &mut TcpStream, backoff: &'a Backoff, run: Duration) -> Progress<'a> {
    let chunk = vec![7u8; CHUNK];
    let mut progress = Progress::new(backoff);
    let until = Instant::now() + run;
    while Instant::now() < until {
        client.write_all(&chunk).await.unwrap();
        progress.mark();
    }
    client.shutdown().await.unwrap();
    progress
}

async fn drain(listener: TcpListener, pause_after_first: Duration) {
    let (mut conn, _) = listener.accept().await.unwrap();
    let mut buf = vec![0u8; CHUNK];
    let mut first = true;
    loop {
        let n = conn.read(&mut buf).await.unwrap();
        if n == 0 {
            return;
        }
        if first {
            first = false;
            tokio::time::sleep(pause_after_first).await;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reader_that_stops_reading_is_still_a_stall() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let sink = tokio::spawn(drain(listener, PAUSE));
    let mut client = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let backoff = Backoff::watch(vec![port, client.local_addr().unwrap().port()]);
    let worst = send_for(&mut client, &backoff, Duration::from_secs(6))
        .await
        .worst;
    sink.await.unwrap();
    eprintln!("planted pause: {worst:?}");
    assert!(
        worst.raw >= PAUSE.mul_f32(0.8),
        "the pause did not show: {worst:?}"
    );
    assert!(
        worst.own() >= BOUND,
        "a reader that paused for {PAUSE:?} passed the bar: {worst:?}"
    );
    // flake-ok: `kernel` is the part of the pause the meter put down to retransmission backoff, read
    // from the `/proc/net/tcp` timers, not a stopwatch: a closed window has none (0 ns measured
    // under 16 busy loops on 4 cores); a load delay moves the raw gap, not this part.
    assert!(
        worst.kernel < Duration::from_millis(300),
        "a closed window was taken for backoff: {worst:?}"
    );
}

/// Runs in the network namespace made by [`a_kernel_drop_and_its_backoff_are_not_a_stall`].
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "runs only inside the namespace the case above makes"]
async fn kernel_drop_inner() {
    if std::env::var_os(INNER).is_none() {
        // skip-ok: a helper role; only the case below runs it, in the namespace it makes
        return;
    }
    println!("INNER-RAN");
    let sh = |cmd: &str| {
        let ok = Command::new("sh")
            .args(["-c", cmd])
            .status()
            .unwrap()
            .success();
        assert!(ok, "{cmd} failed");
    };
    sh("ip link set lo up");
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let sink = tokio::spawn(drain(listener, Duration::ZERO));
    let mut client = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let backoff = Backoff::watch(vec![port, client.local_addr().unwrap().port()]);
    tokio::spawn(async move {
        // The test's timeline: the link works for 0.5 s, then is cut for DROP_FOR.
        tokio::time::sleep(Duration::from_millis(500)).await;
        sh("iptables -A INPUT -i lo -j DROP");
        // The test's timeline: the cut lasts DROP_FOR, then the link is restored.
        tokio::time::sleep(DROP_FOR).await;
        sh("iptables -F INPUT");
    });
    let progress = send_for(&mut client, &backoff, Duration::from_secs(14)).await;
    sink.await.unwrap();
    // The drop is the longest gap by wall clock. On a loaded host another gap can have more own
    // time than the drop's remainder (the meter's `worst`), so the case reads the drop's gap and
    // then holds every gap, the drop's included, to the bound.
    let (drop, worst) = (progress.longest, progress.worst);
    println!("planted drop: {drop:?}, worst by own time: {worst:?}");
    assert!(
        drop.raw >= BOUND + BOUND / 2,
        "the kernel did not back off: {drop:?}"
    );
    assert!(
        drop.own() < BOUND,
        "the backoff was not subtracted: {drop:?}"
    );
    assert!(worst.own() < BOUND, "a gap over the bound: {worst:?}");
}

#[test]
fn a_kernel_drop_and_its_backoff_are_not_a_stall() {
    let exe = std::env::current_exe().unwrap();
    let filter = [
        "--exact",
        "stall_meter::kernel_drop_inner",
        "--ignored",
        "--nocapture",
        "--test-threads=1",
    ];
    let mut tried = Vec::new();
    for prefix in [
        &["unshare", "-rn"][..],
        &["sudo", "-n", "unshare", "-n"][..],
    ] {
        let out = Command::new(prefix[0])
            .args(&prefix[1..])
            .arg("env")
            .arg(format!("{INNER}=1"))
            .arg(&exe)
            .args(filter)
            .output()
            .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        if text.contains("INNER-RAN") {
            assert!(out.status.success(), "the planted drop failed:\n{text}");
            eprintln!("{text}");
            // skip-ok: the planted drop ran and passed; this ends the search for a namespace
            return;
        }
        tried.push(format!("{prefix:?}: {text}"));
    }
    panic!("no network namespace could be made:\n{}", tried.join("\n"));
}
