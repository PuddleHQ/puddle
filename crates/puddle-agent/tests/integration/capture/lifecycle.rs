// SPDX-License-Identifier: AGPL-3.0-or-later
//! The rules over time: tampering, agent restarts, Docker, route changes, host down.

use super::*;

#[tokio::test]
async fn a_guest_that_deletes_or_edits_the_table_is_restored_and_reported() {
    if !enter("a_guest_that_deletes_or_edits_the_table_is_restored_and_reported").await {
        // skip-ok: the outer run re-ran this test in a namespace and asserted that it passed
        return;
    }
    let w = world("tamper").await;
    let listing = || async { ok("nft", &["list", "table", "inet", "puddle"]).await };
    let before = listing().await;
    for needle in [
        "chain nat_out",
        "type nat hook output priority dstnat - 10",
        "ip daddr 198.18.0.0/15 meta l4proto tcp redirect to :3129",
        "udp dport 53 dnat ip to 198.18.0.1:53",
        "ct status dnat accept",
        "reject with tcp reset",
        "log group 1291",
    ] {
        assert!(before.contains(needle), "missing {needle:?} in\n{before}");
    }

    ok("nft", &["delete", "table", "inet", "puddle"]).await;
    eventually_async("the table to come back", WAIT, || async {
        attempt("nft", &["list", "table", "inet", "puddle"]).await.0 == Some(0)
    })
    .await;
    eventually_async(
        "the host to be told the table was deleted",
        WAIT,
        || async {
            let told = w.seen.tampered.lock().unwrap().clone();
            told.contains(&true)
                .then_some(())
                .ok_or_else(|| format!("tampered = {told:?}"))
        },
    )
    .await;

    ok("nft", &["flush", "chain", "inet", "puddle", "filter_out"]).await;
    eventually_async(
        "the chain to be refilled and reported as edited",
        WAIT,
        || async {
            let told = w.seen.tampered.lock().unwrap().clone();
            if told.contains(&false) && listing().await.contains("filter_out") {
                Ok(())
            } else {
                Err(format!("tampered = {told:?}"))
            }
        },
    )
    .await;
    let after = listing().await;
    // Handles differ between installs; the rules do not.
    let strip = |text: &str| {
        text.lines()
            .map(|l| l.split(" # handle").next().unwrap_or(l).trim().to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(strip(&before), strip(&after));

    // And it still captures.
    let (success, line) =
        bash(r#"exec 3<>/dev/tcp/192.0.2.7/5432; printf 'ping\n' >&3; read -t 3 l <&3; echo "$l""#)
            .await;
    assert!(success && line.trim() == "ping", "{line}");
    drop(w.agent);
}

#[tokio::test]
async fn the_rules_outlive_the_agent_and_come_back_with_the_same_stand_ins() {
    if !enter("the_rules_outlive_the_agent_and_come_back_with_the_same_stand_ins").await {
        // skip-ok: the outer run re-ran this test in a namespace and asserted that it passed
        return;
    }
    guest_network().await;
    let dir = common::TempDir::new("kill9");
    let seen = Arc::new(Seen::default());
    let socket = dir.0.join("route.sock");
    start_host(&socket, &seen);
    let table = dir.0.join("table");
    let start = || {
        let mut command = Command::new(env!("CARGO_BIN_EXE_puddle-agent"));
        command.env_clear();
        if let Some(path) = std::env::var_os("LLVM_PROFILE_FILE") {
            command.env("LLVM_PROFILE_FILE", path);
        }
        command
            .env(
                "PUDDLE_AGENT_TARGET",
                format!("unix://{}", socket.display()),
            )
            .env("PUDDLE_AGENT_OOM", "0")
            .env("PUDDLE_AGENT_BRIDGE", "off")
            .env("PUDDLE_AGENT_LISTEN", "127.0.0.1:0")
            .env("PUDDLE_AGENT_CAPTURE", "on")
            .env("PUDDLE_AGENT_DNS_TABLE", &table)
            .env("PUDDLE_AGENT_CAPTURE_POLL_MS", "100")
            .kill_on_drop(true)
            .spawn()
            .unwrap()
    };
    let mut agent = start();
    ready().await;
    let first = ok("dig", &["+short", "@198.18.0.1", "db.test"]).await;
    let handles = |text: &str| text.matches("# handle").count();
    let rules = ok("nft", &["-a", "list", "table", "inet", "puddle"]).await;
    assert!(handles(&rules) > 10, "{rules}");

    agent.kill().await.unwrap();
    agent.wait().await.unwrap();
    // Nobody is listening now, yet nothing escapes: a captured connect is refused at once.
    // The connect is refused at once; one nobody answers would outlast `run`'s limit, which names it.
    let (success, _) = bash("exec 3<>/dev/tcp/192.0.2.7/5432").await;
    assert!(!success);
    assert!(
        run(
            "nft",
            &["list", "table", "inet", "puddle"],
            Duration::from_secs(5)
        )
        .await
        .0,
        "the table is the kernel's, it stays"
    );

    let mut agent = start();
    eventually_async("the restarted agent to answer", WAIT, || async {
        attempt(
            "dig",
            &["+short", "+time=1", "+tries=1", "@198.18.0.1", "db.test"],
        )
        .await
        .1 == first
    })
    .await;
    let (success, line) =
        bash(r#"exec 3<>/dev/tcp/192.0.2.7/5432; printf 'ping\n' >&3; read -t 3 l <&3; echo "$l""#)
            .await;
    assert!(success && line.trim() == "ping", "{line}");
    // Installed once more by the new agent, not stacked on the old rules.
    let again = ok("nft", &["-a", "list", "table", "inet", "puddle"]).await;
    assert_eq!(handles(&again), handles(&rules));
    agent.kill().await.unwrap();
}

#[tokio::test]
async fn capture_leaves_dockers_own_tables_alone_whichever_starts_first() {
    if !enter("capture_leaves_dockers_own_tables_alone_whichever_starts_first").await {
        // skip-ok: the outer run re-ran this test in a namespace and asserted that it passed
        return;
    }
    guest_network().await;
    // What dockerd's iptables-nft leaves behind, in kind: a nat table at the usual priority with
    // a published-port rule and masquerade.
    let docker = [
        "add table ip docker_like",
        "add chain ip docker_like PREROUTING { type nat hook prerouting priority dstnat; }",
        "add chain ip docker_like POSTROUTING { type nat hook postrouting priority srcnat; }",
        "add rule ip docker_like PREROUTING tcp dport 8080 dnat to 172.17.0.2:80",
        "add rule ip docker_like POSTROUTING ip saddr 172.17.0.0/16 oifname != docker0 masquerade",
    ];
    for line in docker {
        ok("nft", &[line]).await;
    }
    let theirs = || async { ok("nft", &["list", "table", "ip", "docker_like"]).await };
    let before = theirs().await;

    let dir = common::TempDir::new("docker");
    let seen = Arc::new(Seen::default());
    let socket = dir.0.join("route.sock");
    start_host(&socket, &seen);
    let agent = Agent::start(config(&socket, &dir.0.join("table")))
        .await
        .unwrap();
    ready().await;
    assert_eq!(
        before,
        theirs().await,
        "installing capture changed Docker's table"
    );

    // dockerd starting after capture adds its chains and rules; ours stay and still work.
    ok(
        "nft",
        &[
            "add",
            "chain",
            "ip",
            "docker_like",
            "OUTPUT",
            "{ type nat hook output priority -100; }",
        ],
    )
    .await;
    ok(
        "nft",
        &[
            "add",
            "rule",
            "ip",
            "docker_like",
            "OUTPUT",
            "ip",
            "daddr",
            "!=",
            "127.0.0.0/8",
            "return",
        ],
    )
    .await;
    // Absence window: a change to another table is not tampering, however long the watcher looks.
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(
        seen.tampered.lock().unwrap().is_empty(),
        "another table's change is not tampering"
    );
    let (success, line) =
        bash(r#"exec 3<>/dev/tcp/192.0.2.7/5432; printf 'ping\n' >&3; read -t 3 l <&3; echo "$l""#)
            .await;
    assert!(success && line.trim() == "ping", "{line}");

    // A table of the same name in another family is not ours to compare or restore.
    ok("nft", &["add", "table", "ip", "puddle"]).await;
    ok(
        "nft",
        &[
            "add",
            "chain",
            "ip",
            "puddle",
            "c",
            "{ type filter hook input priority 0; }",
        ],
    )
    .await;
    // Absence window: a change to another table is not tampering, however long the watcher looks.
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(
        seen.tampered.lock().unwrap().is_empty(),
        "an `ip puddle` table is not ours"
    );
    ok("nft", &["delete", "table", "ip", "puddle"]).await;

    // And capture rewritten (as after a restart) leaves theirs as they were.
    let theirs_now = theirs().await;
    ok("nft", &["delete", "table", "inet", "puddle"]).await;
    eventually_async("the table to come back", WAIT, || async {
        attempt("nft", &["list", "table", "inet", "puddle"]).await.0 == Some(0)
    })
    .await;
    assert_eq!(theirs_now, theirs().await);
    drop(agent);
}

#[tokio::test]
async fn with_capture_off_nothing_is_installed_and_nothing_listens() {
    if !enter("with_capture_off_nothing_is_installed_and_nothing_listens").await {
        // skip-ok: the outer run re-ran this test in a namespace and asserted that it passed
        return;
    }
    guest_network().await;
    let dir = common::TempDir::new("off");
    let seen = Arc::new(Seen::default());
    let socket = dir.0.join("route.sock");
    start_host(&socket, &seen);
    let mut config = config(&socket, &dir.0.join("table"));
    config.capture = None;
    config.dns = None;
    let agent = Agent::start(config).await.unwrap();
    // Absence window: long enough for startup to have opened a listener, had capture been on.
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(agent.capture_addr(), None);
    assert!(
        !run(
            "nft",
            &["list", "table", "inet", "puddle"],
            Duration::from_secs(5)
        )
        .await
        .0,
        "a table appeared with capture off"
    );
    let (_, ifaces) = run("ip", &["-br", "link"], Duration::from_secs(5)).await;
    assert!(!ifaces.contains("dummy0"), "{ifaces}");
    let (success, _) = bash("exec 3<>/dev/tcp/127.0.0.1/3129").await;
    assert!(!success, "something listens on the capture port");
    assert!(seen.connects.lock().unwrap().is_empty());
    let _ = Ipv4Addr::LOCALHOST;
}

#[tokio::test]
async fn a_default_route_that_moves_gets_the_rules_rewritten_for_the_new_interface() {
    if !enter("a_default_route_that_moves_gets_the_rules_rewritten_for_the_new_interface").await {
        // skip-ok: the outer run re-ran this test in a namespace and asserted that it passed
        return;
    }
    let w = world("moved").await;
    assert!(
        ok("nft", &["list", "table", "inet", "puddle"])
            .await
            .contains("oifname \"eth0\"")
    );
    for args in [
        &["link", "add", "eth1", "type", "dummy"][..],
        &["addr", "add", "172.16.1.6/30", "dev", "eth1"],
        &["link", "set", "eth1", "up"],
        &[
            "route",
            "replace",
            "default",
            "via",
            "172.16.1.5",
            "dev",
            "eth1",
        ],
    ] {
        ok("ip", args).await;
    }
    eventually_async("the rules to follow the default route", WAIT, || async {
        let (_, text) = attempt("nft", &["list", "table", "inet", "puddle"]).await;
        text.contains("oifname \"eth1\"") && !text.contains("oifname \"eth0\"")
    })
    .await;
    let (success, line) =
        bash(r#"exec 3<>/dev/tcp/192.0.2.7/5432; printf 'ping\n' >&3; read -t 3 l <&3; echo "$l""#)
            .await;
    assert!(success && line.trim() == "ping", "{line}");
    assert!(
        w.seen.tampered.lock().unwrap().is_empty(),
        "a network change is not tampering"
    );
}

#[tokio::test]
async fn a_guest_with_no_default_route_gets_one_through_the_dummy_interface() {
    if !enter("a_guest_with_no_default_route_gets_one_through_the_dummy_interface").await {
        // skip-ok: the outer run re-ran this test in a namespace and asserted that it passed
        return;
    }
    ok("ip", &["link", "set", "lo", "up"]).await;
    let dir = common::TempDir::new("noroute");
    let seen = Arc::new(Seen::default());
    let socket = dir.0.join("route.sock");
    start_host(&socket, &seen);
    let _agent = Agent::start(config(&socket, &dir.0.join("table")))
        .await
        .unwrap();
    ready().await;
    let routes = ok("ip", &["route", "show", "default"]).await;
    assert!(routes.contains("dev dummy0"), "{routes}");
    assert!(
        ok("nft", &["list", "table", "inet", "puddle"])
            .await
            .contains("oifname \"dummy0\"")
    );
    // Without the route this connect would fail with "network unreachable" before any rule saw it.
    let (success, line) =
        bash(r#"exec 3<>/dev/tcp/192.0.2.7/5432; printf 'ping\n' >&3; read -t 3 l <&3; echo "$l""#)
            .await;
    assert!(success && line.trim() == "ping", "{line}");
    assert!(
        seen.connects
            .lock()
            .unwrap()
            .iter()
            .any(|c| c.authority == "192.0.2.7:5432")
    );
}

#[tokio::test]
async fn with_the_host_down_a_captured_connection_is_reset_not_hung() {
    if !enter("with_the_host_down_a_captured_connection_is_reset_not_hung").await {
        // skip-ok: the outer run re-ran this test in a namespace and asserted that it passed
        return;
    }
    guest_network().await;
    let dir = common::TempDir::new("hostdown");
    // Nothing listens on the route: the agent cannot reach the host.
    let _agent = Agent::start(config(&dir.0.join("nobody.sock"), &dir.0.join("table")))
        .await
        .unwrap();
    eventually_async("the capture table", WAIT, || async {
        puddle_agent::capture::nft::read().is_ok_and(|t| t.is_some())
    })
    .await;
    bash_not_hung("exec 3<>/dev/tcp/192.0.2.7/5432; read -t 5 l <&3").await;
}

/// The check the three "reset, not hung" cases above rely on: a peer that holds the connection and
/// never answers ends the shell's read with its timeout status, a peer that resets or closes does
/// not. (The old check, `!success || took < N`, passed for the hung peer: `read -t` failing is
/// `!success`.)
#[tokio::test]
async fn a_read_that_waits_out_its_timeout_is_told_from_a_reset() {
    let silent = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let silent_port = silent.local_addr().unwrap().port();
    let _held = tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = silent.accept().await {
            held.push(stream);
        }
    });
    let closing = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let closing_port = closing.local_addr().unwrap().port();
    let _closer = tokio::spawn(async move {
        while let Ok((stream, _)) = closing.accept().await {
            drop(stream);
        }
    });
    let read = |port: u16| format!("exec 3<>/dev/tcp/127.0.0.1/{port}; read -t 1 l <&3");
    bash_not_hung(&read(closing_port)).await;
    let hung = tokio::spawn(async move { bash_not_hung(&read(silent_port)).await }).await;
    let panic = hung.expect_err("a peer that never answers must fail the check");
    let text = panic
        .into_panic()
        .downcast::<String>()
        .map(|t| *t)
        .unwrap_or_default();
    assert!(text.contains("hung until the read timed out"), "{text}");
}
