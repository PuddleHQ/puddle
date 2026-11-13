// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

const NETSTAT: &str = "
Active Connections

  Proto  Local Address          Foreign Address        State           PID
  TCP    0.0.0.0:135            0.0.0.0:0              LISTENING       1032
  TCP    127.0.0.1:49152        0.0.0.0:0              LISTENING       4100
  TCP    127.0.0.1:49152        127.0.0.1:50000        ESTABLISHED     4100
  TCP    [::]:445               [::]:0                 LISTENING       4
  TCP    [::1]:8080             [::]:0                 LISTENING       4100
  UDP    0.0.0.0:123            *:*                                    2344
  UDP    [fe80::1%12]:1900      *:*                                    2344
  UDP    127.0.0.1:5353         *:*                                    4100
";

#[test]
fn listening_and_bound_sockets_are_read_and_established_ones_are_not() {
    let rows = parse_netstat(NETSTAT).unwrap();
    let shown: Vec<String> = rows.iter().map(ToString::to_string).collect();
    assert_eq!(
        shown,
        [
            "TCP 0.0.0.0:135 (pid 1032)",
            "TCP 127.0.0.1:49152 (pid 4100)",
            "TCP :::445 (pid 4)",
            "TCP ::1:8080 (pid 4100)",
            "UDP 0.0.0.0:123 (pid 2344)",
            "UDP fe80::1:1900 (pid 2344)",
            "UDP 127.0.0.1:5353 (pid 4100)",
        ]
    );
    let open: Vec<bool> = rows.iter().map(|r| !r.is_loopback()).collect();
    assert_eq!(open, [true, false, true, false, true, true, false]);
}

#[test]
fn a_row_that_cannot_be_read_fails_the_sweep() {
    for bad in [
        "  TCP    nonsense               0.0.0.0:0              LISTENING       1",
        "  TCP    0.0.0.0:99999          0.0.0.0:0              LISTENING       1",
        "  TCP    0.0.0.0:80             0.0.0.0:0              LISTENING       x",
        "  UDP    0.0.0.0:80             *:*                                    -1",
        "  UDP    host:80                *:*                                    1",
    ] {
        assert!(parse_netstat(bad).is_err(), "{bad}");
    }
}

#[test]
fn processes_are_read_and_their_descendants_found() {
    let table = parse_processes(
            "10 1 100 test.exe\n11 10 110 msb.exe\n12 11 120 msb helper.exe\n\n20 1 105 other.exe\n13 12 130 deep.exe\n",
        )
        .unwrap();
    assert_eq!(table.len(), 5);
    assert_eq!(table[2].name, "msb helper.exe");
    assert_eq!(descendants(&table, 10), BTreeSet::from([10, 11, 12, 13]));
    assert_eq!(descendants(&table, 20), BTreeSet::from([20]));
    assert_eq!(descendants(&table, 99), BTreeSet::from([99]));
}

#[test]
fn a_reused_parent_id_does_not_pull_in_the_processes_of_its_old_owner() {
    // Pid 10 is the test now; csrss (pid 7) was started by a session manager that had pid
    // 10 earlier and is gone, so csrss was created before the process that holds 10 now.
    let table = parse_processes(
        "10 1 500 test.exe\n7 10 100 csrss.exe\n8 7 101 child.exe\n11 10 600 msb.exe\n",
    )
    .unwrap();
    assert_eq!(descendants(&table, 10), BTreeSet::from([10, 11]));
}

#[test]
fn a_process_row_that_cannot_be_read_is_an_error() {
    for bad in [
        "x 1 5 a.exe",
        "1 x 5 a.exe",
        "1 2 x a.exe",
        "1 2 5",
        "1 2",
        "1",
    ] {
        assert!(parse_processes(bad).is_err(), "{bad}");
    }
}
