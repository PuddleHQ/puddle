// SPDX-License-Identifier: AGPL-3.0-or-later
use std::net::SocketAddr;

use axum::Router;
#[cfg(target_os = "linux")]
use axum::response::sse::{Event, Sse};
use axum::routing::get as get_route;
use puddle_test_kit::eventually;
use tokio::net::TcpListener;

use super::*;

async fn serve(app: Router) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

fn target(addr: SocketAddr) -> Target {
    Target {
        addr: addr.to_string(),
        token: None,
    }
}

#[test]
fn counts_come_from_a_list_or_a_number() {
    assert_eq!(count_in(br#"{"requests":[1,2,3]}"#, "requests"), Ok(3.0));
    assert_eq!(count_in(br#"{"next_after":812}"#, "next_after"), Ok(812.0));
    assert!(count_in(br#"{"a":1}"#, "b").unwrap_err().contains("no `b`"));
    assert!(
        count_in(br#"{"a":"x"}"#, "a")
            .unwrap_err()
            .contains("neither")
    );
    assert!(count_in(b"nope", "a").is_err());
}

#[test]
fn the_w4_share_covers_only_the_window_and_is_none_when_empty() {
    let mut w = W4Window::default();
    assert_eq!(w.fail_share(1000, 300), None);
    w.record(100, false);
    w.record(800, true);
    w.record(900, true);
    w.record(950, false);
    // At 1000 the request at 100 is older than 300 s and is dropped: 1 of 3 failed.
    let share = w.fail_share(1000, 300).unwrap();
    assert!((share - 1.0 / 3.0).abs() < 1e-9, "{share}");
    assert_eq!(w.fail_share(5000, 300), None);
}

#[test]
fn the_host_files_are_read_into_numbers() {
    assert_eq!(
        available_ram_kb("MemTotal: 1 kB\nMemAvailable:   2048 kB\n"),
        Some(2048.0)
    );
    assert_eq!(available_ram_kb("MemTotal: 1 kB\n"), None);
    // user nice system idle iowait: busy = 10+0+5, idle = 80+5.
    assert_eq!(
        cpu_jiffies("cpu  10 0 5 80 5 0 0 0 0 0\ncpu0 1 1 1 1\n"),
        Some((15, 100))
    );
    assert_eq!(cpu_jiffies("intr 1\n"), None);
    assert_eq!(
        parse_df(
            "Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/sda 100 40 60 40% /\n"
        ),
        Some(60.0)
    );
    assert_eq!(parse_df("only a header\n"), None);
}

#[cfg(target_os = "linux")]
#[test]
fn this_process_has_memory_threads_and_descriptors() {
    let got = read_process(Path::new("/proc/self")).unwrap();
    let names: Vec<_> = got.iter().map(|(n, _)| *n).collect();
    assert_eq!(names, ["rss_kb", "threads", "handles"]);
    assert!(got[0].1 > 100.0, "{got:?}");
    assert!(got[1].1 >= 1.0);
    assert!(
        got[2].1 >= 3.0,
        "stdin, stdout and stderr at least: {got:?}"
    );
    assert!(read_process(Path::new("/proc/0")).is_none());
}

#[cfg(target_os = "linux")]
#[test]
fn a_process_whose_descriptors_cannot_be_read_still_has_memory_and_threads() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("status"),
        "VmRSS:\t  2048 kB\nThreads:\t7\n",
    )
    .unwrap();
    assert_eq!(
        read_process(dir.path()),
        Some(vec![("rss_kb", 2048.0), ("threads", 7.0)])
    );
}

#[test]
fn processes_are_found_by_command_name_prefix() {
    let dir = tempfile::tempdir().unwrap();
    for (pid, comm) in [
        ("12", "puddle-worker\n"),
        ("13", "bash\n"),
        ("14", "msb-vm\n"),
    ] {
        std::fs::create_dir(dir.path().join(pid)).unwrap();
        std::fs::write(dir.path().join(pid).join("comm"), comm).unwrap();
    }
    std::fs::create_dir(dir.path().join("self")).unwrap();
    // a process that ended between the listing and the read has a directory but no `comm`
    std::fs::create_dir(dir.path().join("15")).unwrap();
    let prefixes = ["puddle".to_owned(), "msb".to_owned()];
    assert_eq!(
        find_processes(dir.path(), &prefixes),
        [("msb-vm".to_owned(), 14), ("puddle-worker".to_owned(), 12)]
    );
    assert_eq!(find_processes(&dir.path().join("none"), &prefixes), []);
}

#[test]
fn a_prefix_ending_in_a_dollar_is_a_whole_name() {
    let dir = tempfile::tempdir().unwrap();
    for (pid, comm) in [("12", "puddle"), ("13", "puddle-soak")] {
        std::fs::create_dir(dir.path().join(pid)).unwrap();
        std::fs::write(dir.path().join(pid).join("comm"), comm).unwrap();
    }
    assert_eq!(
        find_processes(dir.path(), &["puddle$".to_owned()]),
        [("puddle".to_owned(), 12)]
    );
}

#[tokio::test]
async fn a_get_returns_status_body_and_time_and_sends_the_token() {
    let app = Router::new().route(
        "/api/x",
        get_route(|headers: http::HeaderMap| async move {
            let auth = headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("none")
                .to_owned();
            // simulated latency of the server, so the reply time can be asserted
            tokio::time::sleep(Duration::from_millis(50)).await;
            auth
        }),
    );
    let addr = serve(app).await;
    let mut t = target(addr);
    t.token = Some("abc".to_owned());
    let reply = get(&t, "/api/x").await.unwrap();
    assert_eq!(reply.status, 200);
    assert_eq!(reply.body, b"Bearer abc");
    assert!(reply.took >= Duration::from_millis(50), "{:?}", reply.took);
    assert_eq!(get(&t, "/api/none").await.unwrap().status, 404);
    let dead = Target {
        addr: "127.0.0.1:1".to_owned(),
        token: None,
    };
    assert!(get(&dead, "/").await.is_err());
}

/// A server that answers every connection by writing `script` and then closing.
async fn scripted(
    script: &'static str,
    hold: Duration,
) -> (SocketAddr, Arc<std::sync::atomic::AtomicU32>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let connections = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let seen = connections.clone();
    tokio::spawn(async move {
        // flake-ok: a test server's accept loop, ended with the runtime, not a wait for a condition
        loop {
            let (mut s, _) = listener.accept().await.unwrap();
            seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            tokio::spawn(async move {
                let mut buf = [0_u8; 1024];
                let _ = s.read(&mut buf).await;
                // A `<pause>` in the script is 300 ms of silence.
                let (first, more) = script.split_once("<pause>").unwrap_or((script, ""));
                s.write_all(first.as_bytes()).await.unwrap();
                if !more.is_empty() {
                    // simulated latency: 300 ms of silence between the comment and the data line
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    s.write_all(more.as_bytes()).await.unwrap();
                }
                // holds the connection open so the reader sees no end of the stream
                tokio::time::sleep(hold).await;
            });
        }
    });
    (addr, connections)
}

#[tokio::test]
async fn keep_alive_comments_are_not_events_and_data_lines_are() {
    let (addr, _) = scripted(
        "HTTP/1.0 200 OK\r\n\r\n: keep-alive\n\n",
        Duration::from_secs(30),
    )
    .await;
    let state = SseState::new();
    let task = tokio::spawn(read_events(target(addr), state.clone()));
    // an absence window: a comment line must not reset the age over 400 ms
    tokio::time::sleep(Duration::from_millis(400)).await;
    let (age, reconnects) = state.read();
    assert!(age >= 0.4, "a comment must not reset the age: {age}");
    assert_eq!(reconnects, 0);
    task.abort();

    let (addr, _) = scripted(
        "HTTP/1.0 200 OK\r\n\r\n: keep-alive\n\n<pause>data: {}\n\n",
        Duration::from_secs(30),
    )
    .await;
    let state = SseState::new();
    let task = tokio::spawn(read_events(target(addr), state.clone()));
    // the data line arrives at 300 ms; the age is read 150 ms after it
    tokio::time::sleep(Duration::from_millis(450)).await;
    let age = state.read().0;
    assert!(age < 0.3, "the data line at 300 ms resets the age: {age}");
    task.abort();
}

#[tokio::test]
async fn a_stream_that_ends_is_reconnected_and_counted() {
    let (addr, connections) = scripted("HTTP/1.0 200 OK\r\n\r\ndata: {}\n\n", Duration::ZERO).await;
    let state = SseState::new();
    let task = tokio::spawn(read_events(target(addr), state.clone()));
    // the server closes at once; 2.6 s lets the reader's 1 s pause before a reconnect run twice
    tokio::time::sleep(Duration::from_millis(2600)).await;
    task.abort();
    let n = connections.load(std::sync::atomic::Ordering::SeqCst);
    assert!(n >= 3, "{n} connections in 2.6 s");
    assert!(state.read().1 >= 2, "{:?}", state.read());
}

#[tokio::test]
async fn the_prober_records_a_good_and_a_failed_request() {
    let app = Router::new().route("/", get_route(|| async { "ok" }));
    let addr = serve(app).await;
    let window = Arc::new(Mutex::new(W4Window::default()));
    let good = tokio::spawn(probe_web_app(
        target(addr),
        "/".to_owned(),
        Duration::from_millis(50),
        window.clone(),
        || 10,
    ));
    // the prober records no signal a test could wait on: 200 ms is four rounds at a 50 ms pace
    tokio::time::sleep(Duration::from_millis(200)).await;
    good.abort();
    assert_eq!(window.lock().unwrap().fail_share(10, 300), Some(0.0));
    let bad = tokio::spawn(probe_web_app(
        target(addr),
        "/missing".to_owned(),
        Duration::from_millis(50),
        window.clone(),
        || 10,
    ));
    // the prober records no signal a test could wait on: 200 ms is four rounds at a 50 ms pace
    tokio::time::sleep(Duration::from_millis(200)).await;
    bad.abort();
    assert!(window.lock().unwrap().fail_share(10, 300).unwrap() > 0.0);
}

#[cfg(target_os = "linux")]
fn fake_api() -> Router {
    Router::new()
        .route("/api/health", get_route(|| async { "ok" }))
        .route("/api/workspaces", get_route(|| async { "[]" }))
        .route(
            "/api/pending",
            get_route(|| async { r#"{"requests":[1,2]}"# }),
        )
        .route(
            "/api/audit",
            get_route(|| async { r#"{"entries":[],"next_after":500,"next_before":null}"# }),
        )
        .route(
            "/api/problems",
            get_route(|| async { r#"{"problems":[1]}"# }),
        )
        .route(
            "/api/events",
            get_route(|| async {
                Sse::new(futures_util::stream::once(async {
                    Ok::<_, std::convert::Infallible>(Event::default().data("x"))
                }))
            }),
        )
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_sample_holds_every_family_of_the_measurement_list() {
    let api = serve(fake_api()).await;
    let web = serve(Router::new().route("/", get_route(|| async { "up" }))).await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("puddle.db"), vec![0; 4096]).unwrap();
    std::fs::write(dir.path().join("puddle.db-wal"), vec![0; 100]).unwrap();
    std::fs::write(dir.path().join("vol.img"), vec![0; 10]).unwrap();
    let comm = std::fs::read_to_string("/proc/self/comm").unwrap();
    let cfg = Config {
        api: target(api),
        w4: Some(target(web)),
        process_prefixes: vec![comm.trim().to_owned()],
        db: Some(dir.path().join("puddle.db")),
        files: vec![("vol_qa".to_owned(), dir.path().join("vol.img"))],
        disk: Some(dir.path().to_owned()),
    };
    let mut sampler = Sampler::start(cfg, || 7);
    // an absence window: the sampler's two readers need 300 ms to see the fake servers' first lines
    tokio::time::sleep(Duration::from_millis(300)).await;
    let mut s = Series::default();
    sampler.sample(100, &mut s).await;
    // the CPU reading needs /proc/stat to have moved on by a jiffy (10 ms) between the two samples
    tokio::time::sleep(Duration::from_millis(50)).await;
    sampler.sample(400, &mut s).await;
    let value = |m: &str| s.points(m).first().map(|p| p.1);
    assert_eq!(value("file.db.bytes"), Some(4096.0));
    assert_eq!(value("file.db_wal.bytes"), Some(100.0));
    assert_eq!(value("file.vol_qa.bytes"), Some(10.0));
    assert_eq!(value("count.pending"), Some(2.0));
    assert_eq!(value("count.audit"), Some(500.0));
    assert_eq!(value("count.notices"), Some(1.0));
    assert_eq!(value("w4.fail_share"), Some(0.0));
    for m in [
        "api.health.ms",
        "api.workspaces.ms",
        "api.pending.ms",
        "api.audit.ms",
        "sse.age_s",
        "sse.reconnects",
        "host.free_ram_kb",
        "host.free_disk_kb",
    ] {
        assert!(value(m).is_some(), "{m} missing");
    }
    assert!(s.metrics("proc.", ".rss_kb").next().is_some());
    assert!(s.metrics("proc.", ".handles").next().is_some());
    assert!(s.metrics("proc.", ".threads").next().is_some());
    // CPU needs two samples.
    assert_eq!(s.points("host.cpu_pct").len(), 1);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_reading_that_fails_leaves_a_gap_never_a_zero() {
    let dead = Target {
        addr: "127.0.0.1:1".to_owned(),
        token: None,
    };
    let cfg = Config {
        api: dead,
        w4: None,
        process_prefixes: vec![],
        db: Some("/nonexistent/puddle.db".into()),
        files: vec![],
        disk: None,
    };
    let mut sampler = Sampler::start(cfg, || 7);
    let mut s = Series::default();
    sampler.sample(100, &mut s).await;
    assert_eq!(s.points("api.health.ms"), []);
    assert_eq!(s.points("file.db.bytes"), []);
    assert_eq!(s.points("w4.fail_share"), []);
    assert_ne!(s.points("host.free_ram_kb"), []);
}

#[tokio::test]
async fn a_reader_that_cannot_connect_keeps_trying_and_counts_each_try() {
    let dead = Target {
        addr: "127.0.0.1:1".to_owned(),
        token: None,
    };
    let state = SseState::new();
    let task = tokio::spawn(read_events(dead, state.clone()));
    // A refused connect to a closed port fails at once on Linux but takes about 2 s on Windows
    // (the stack retries the SYN), so wait for the first counted try instead of a fixed time.
    eventually("one counted try", Duration::from_secs(20), || {
        let seen = state.read();
        if seen.1 >= 1 {
            Ok(())
        } else {
            Err(format!("{seen:?}"))
        }
    })
    .await;
    task.abort();
}

/// A restart of `puddle serve` moves the API (a new token, maybe a new port): after `retarget`
/// the timed calls go to the new one.
#[tokio::test]
async fn a_retargeted_sampler_reads_the_new_api() {
    let api = serve(Router::new().route("/api/health", get_route(|| async { "{}" }))).await;
    let cfg = Config {
        api: Target {
            addr: "127.0.0.1:1".to_owned(),
            token: None,
        },
        w4: None,
        process_prefixes: vec![],
        db: None,
        files: vec![],
        disk: None,
    };
    let mut sampler = Sampler::start(cfg, || 7);
    let mut before = Series::default();
    sampler.sample(100, &mut before).await;
    assert_eq!(before.points("api.health.ms"), []);
    sampler.retarget(target(api));
    let mut after = Series::default();
    sampler.sample(400, &mut after).await;
    assert_eq!(after.points("api.health.ms").len(), 1, "{}", after.render());
}

/// Two runs on one machine each measure their own server: a process of the same name outside
/// the run's tree is left out.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn only_the_processes_of_the_runs_own_tree_are_measured() {
    let mut ours = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let mut other = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let cfg = Config {
        api: Target {
            addr: "127.0.0.1:1".to_owned(),
            token: None,
        },
        w4: None,
        process_prefixes: vec!["sleep$".to_owned()],
        db: None,
        files: vec![],
        disk: None,
    };
    let mut sampler = Sampler::start(cfg, || 7);
    let mut all = Series::default();
    sampler.sample(100, &mut all).await;
    sampler.only_below(Some(ours.id()));
    let mut mine = Series::default();
    sampler.sample(100, &mut mine).await;
    for child in [&mut ours, &mut other] {
        child.kill().unwrap();
        child.wait().unwrap();
    }
    let rss = |s: &Series| {
        s.metrics("proc.sleep.", ".rss_kb")
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    assert!(
        rss(&all).contains(&format!("proc.sleep.{}.rss_kb", other.id())),
        "{:?}",
        rss(&all)
    );
    assert_eq!(rss(&mine), [format!("proc.sleep.{}.rss_kb", ours.id())]);
}
