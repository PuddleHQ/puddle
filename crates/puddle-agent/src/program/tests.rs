// SPDX-License-Identifier: AGPL-3.0-or-later
use std::os::unix::fs::symlink;

use super::*;

/// A fake `/proc` with one TCP table and a process list.
struct Proc(tempfile::TempDir);

impl Proc {
    fn new(tcp: &str, tcp6: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("net")).unwrap();
        let header = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n";
        fs::write(dir.path().join("net/tcp"), format!("{header}{tcp}")).unwrap();
        fs::write(dir.path().join("net/tcp6"), format!("{header}{tcp6}")).unwrap();
        Self(dir)
    }

    /// A process `pid` holding `socket:[inode]`, with the given `exe` target and `comm`.
    fn process(&self, pid: u32, inode: u64, exe: &str, comm: &str) {
        let dir = self.0.path().join(pid.to_string());
        fs::create_dir_all(dir.join("fd")).unwrap();
        symlink("/dev/null", dir.join("fd/0")).unwrap();
        symlink(format!("socket:[{inode}]"), dir.join("fd/3")).unwrap();
        symlink(exe, dir.join("exe")).unwrap();
        fs::write(dir.join("comm"), format!("{comm}\n")).unwrap();
    }

    fn resolver(&self) -> Resolver {
        Resolver::new(self.0.path())
    }
}

fn row(local: &str, inode: u64) -> String {
    format!(
        "   0: {local} 0100007F:1F90 01 00000000:00000000 00:00000000 00000000  1000        0 {inode} 1 0000000000000000 100 0 0 10 0\n"
    )
}

fn peer(text: &str) -> SocketAddr {
    text.parse().unwrap()
}

#[test]
fn an_address_in_a_table_row_is_read_in_the_kernels_byte_order() {
    assert_eq!(parse_address("0100007F:1F90"), Some(peer("127.0.0.1:8080")));
    assert_eq!(
        parse_address("0F02000A:A028"),
        Some(peer("10.0.2.15:41000"))
    );
    assert_eq!(
        parse_address("00000000000000000000000001000000:0050"),
        Some(peer("[::1]:80"))
    );
    assert_eq!(
        parse_address("0000000000000000FFFF00000100007F:0050"),
        Some(peer("[::ffff:127.0.0.1]:80"))
    );
    assert_eq!(parse_address("nonsense"), None);
    assert_eq!(parse_address("0100007F:ZZ"), None);
}

#[test]
fn a_peer_resolves_to_the_exe_name_of_the_process_that_holds_its_socket() {
    let proc = Proc::new(&row("0100007F:A028", 4242), "");
    proc.process(7, 1, "/usr/bin/cat", "cat");
    proc.process(812, 4242, "/usr/bin/curl", "curl");
    let name = proc.resolver().resolve(peer("127.0.0.1:41000")).unwrap();
    assert_eq!(name.as_str(), "curl");
}

#[test]
fn a_v4_peer_is_found_in_the_v6_table_as_a_mapped_address() {
    let proc = Proc::new("", &row("0000000000000000FFFF00000100007F:A028", 99));
    proc.process(5, 99, "/usr/bin/node", "MainThread");
    let name = proc.resolver().resolve(peer("127.0.0.1:41000")).unwrap();
    assert_eq!(name.as_str(), "node");
    // The same connection seen on a dual-stack listener.
    let name = proc
        .resolver()
        .resolve(peer("[::ffff:127.0.0.1]:41000"))
        .unwrap();
    assert_eq!(name.as_str(), "node");
}

#[test]
fn a_peer_that_is_in_no_table_has_no_program() {
    let proc = Proc::new(&row("0100007F:A028", 4242), "");
    proc.process(812, 4242, "/usr/bin/curl", "curl");
    // Another port, and a container's address.
    assert_eq!(proc.resolver().resolve(peer("127.0.0.1:41001")), None);
    assert_eq!(proc.resolver().resolve(peer("172.17.0.2:41000")), None);
}

#[test]
fn a_socket_nobody_holds_has_no_program() {
    let proc = Proc::new(&row("0100007F:A028", 4242), "");
    proc.process(7, 1, "/usr/bin/cat", "cat");
    assert_eq!(proc.resolver().resolve(peer("127.0.0.1:41000")), None);
}

#[test]
fn a_script_run_by_its_shebang_is_named_after_the_script() {
    assert_eq!(pick_name(Some("python3.15"), Some("pip")), "pip");
    assert_eq!(pick_name(Some("python3"), Some("python3")), "python3");
    assert_eq!(pick_name(Some("bash"), Some("deploy.sh")), "deploy.sh");
}

#[test]
fn a_binary_keeps_its_exe_name_over_its_comm() {
    // node renames its main thread; a compiled tool may set any comm.
    assert_eq!(pick_name(Some("node"), Some("MainThread")), "node");
    assert_eq!(pick_name(Some("node"), Some("npm")), "node");
    assert_eq!(pick_name(Some("python3"), Some("MainThread")), "python3");
    // `pythonista` is not an interpreter name.
    assert_eq!(pick_name(Some("pythonista"), Some("x")), "pythonista");
}

#[test]
fn comm_is_the_fallback_when_exe_is_unreadable() {
    assert_eq!(pick_name(None, Some("curl")), "curl");
    assert_eq!(pick_name(Some("curl"), Some("")), "curl");
    assert_eq!(pick_name(None, None), "");
}

#[test]
fn a_replaced_binary_loses_its_deleted_marker_and_the_name_is_cleaned() {
    let proc = Proc::new(&row("0100007F:A028", 4242), "");
    proc.process(812, 4242, "/usr/bin/my tool\r\n (deleted)", "x");
    let name = proc.resolver().resolve(peer("127.0.0.1:41000")).unwrap();
    assert_eq!(name.as_str(), "mytool");
}

#[test]
fn a_process_name_is_read_once_inside_the_cache_window() {
    let proc = Proc::new(&row("0100007F:A028", 4242), "");
    proc.process(812, 4242, "/usr/bin/python3", "pip");
    let resolver = proc.resolver();
    assert_eq!(
        resolver.resolve(peer("127.0.0.1:41000")).unwrap().as_str(),
        "pip"
    );
    // The process renames itself; the cached name stays for the window.
    fs::write(proc.0.path().join("812/comm"), "other\n").unwrap();
    assert_eq!(
        resolver.resolve(peer("127.0.0.1:41000")).unwrap().as_str(),
        "pip"
    );
}

#[test]
fn a_row_with_a_cut_line_is_skipped() {
    assert_eq!(
        inode_of_row("   0: 0100007F:A028", peer("127.0.0.1:41000")),
        None
    );
    assert_eq!(inode_of_row("", peer("127.0.0.1:41000")), None);
}

#[tokio::test]
async fn a_real_loopback_connection_resolves_to_this_process() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dial = TcpStream::connect(listener.local_addr().unwrap());
    let (client, accepted) = tokio::join!(dial, listener.accept());
    let (accepted, _) = accepted.unwrap();
    let client = client.unwrap();
    let ours = Resolver::new("/proc").read_name(std::process::id());
    assert!(ours.is_some());
    // The accepted end's peer is the dialling socket, which this process holds.
    assert_eq!(of_client(&accepted).await, ours);
    drop(client);
}
