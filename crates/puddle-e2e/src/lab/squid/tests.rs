// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;

/// A `Squid` around a process that is not Squid, in an empty directory.
fn stand_in(command: &mut Command) -> Squid {
    Squid {
        // Nothing binds or connects to this port: it only names the shared memory the drop removes.
        port: 0,
        prefix: Prefix::new("squid"),
        dir: tempfile::tempdir().unwrap(),
        stop: Stop::Process(command.stdout(Stdio::null()).spawn().unwrap()),
        started: StartCost::default(),
    }
}

#[test]
fn an_empty_access_log_is_explained_with_the_directory_and_cache_log() {
    let squid = stand_in(Command::new("sleep").arg("60"));
    fs::write(squid.dir.path().join("cache.log"), "cache says hi").unwrap();
    let log = squid.access_log();
    assert!(log.starts_with("(empty access log; files "), "{log}");
    assert!(log.contains("cache says hi"), "{log}");
}

#[test]
fn a_squid_with_a_parent_proxy_sends_every_tunnel_to_it() {
    let dir = tempfile::tempdir().unwrap();
    write_config(
        dir.path(),
        dir.path(),
        &SquidOptions::open(&[443]).through(4242),
        3128,
    );
    let conf = read(&dir.path().join("squid.conf"));
    assert!(
        conf.contains(
            "cache_peer 127.0.0.1 parent 4242 0 no-query no-digest\nnever_direct allow all\n"
        ),
        "{conf}"
    );
    write_config(dir.path(), dir.path(), &SquidOptions::open(&[443]), 3128);
    assert!(!read(&dir.path().join("squid.conf")).contains("cache_peer"));
}

#[test]
fn a_line_written_after_the_call_is_waited_for() {
    let squid = stand_in(Command::new("sleep").arg("60"));
    let path = squid.dir.path().join("access.log");
    let writer = std::thread::spawn(move || {
        // flake-ok: simulated latency of Squid's log daemon, the thing the wait covers.
        std::thread::sleep(Duration::from_millis(700));
        fs::write(path, "TCP_TUNNEL/200 x").unwrap();
    });
    assert_eq!(squid.access_log_with("TCP_TUNNEL/200"), "TCP_TUNNEL/200 x");
    writer.join().unwrap();
}

#[test]
fn a_line_that_never_comes_returns_the_log_as_it_stands() {
    let squid = stand_in(Command::new("sleep").arg("60"));
    fs::write(squid.dir.path().join("access.log"), "GET x").unwrap();
    let wait = Duration::from_millis(200);
    let started = Instant::now();
    assert_eq!(squid.access_log_within("TCP_TUNNEL/200", wait), "GET x");
    assert!(started.elapsed() >= wait);
}

#[test]
fn a_written_access_log_is_returned_as_is() {
    let squid = stand_in(Command::new("sleep").arg("60"));
    fs::write(squid.dir.path().join("access.log"), "GET x").unwrap();
    assert_eq!(squid.access_log(), "GET x");
}

#[test]
#[should_panic(expected = "squid.stderr:\nboom\ncache.log:\ncache says hi")]
fn a_squid_that_dies_at_start_shows_both_logs() {
    let mut squid = stand_in(&mut Command::new("false"));
    fs::write(squid.dir.path().join("squid.stderr"), "boom").unwrap();
    fs::write(squid.dir.path().join("cache.log"), "cache says hi").unwrap();
    let why = squid.wait().unwrap_err();
    panic!("{why}");
}
