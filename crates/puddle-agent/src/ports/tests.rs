// SPDX-License-Identifier: AGPL-3.0-or-later
use std::fmt::Write as _;
use std::time::Duration;

use puddle_test_kit::no_event_for;

use super::*;

const HEADER: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n";

fn row(local: &str, state: &str) -> String {
    format!(
        "   0: {local} 00000000:0000 {state} 00000000:00000000 00:00000000 00000000     0        0 1 1 0\n"
    )
}

/// The kernel's text for an address on this machine's byte order.
fn v4(ip: [u8; 4], port: u16) -> String {
    format!("{:08X}:{port:04X}", u32::from_ne_bytes(ip))
}

fn v6(ip: [u8; 16], port: u16) -> String {
    let mut text = String::new();
    for word in ip.as_chunks::<4>().0 {
        write!(text, "{:08X}", u32::from_ne_bytes(*word)).unwrap();
    }
    format!("{text}:{port:04X}")
}

#[test]
fn only_listeners_on_the_wildcard_or_loopback_are_ports_a_forward_can_reach() {
    let mut v6_loopback = [0u8; 16];
    v6_loopback[15] = 1;
    let mut mapped = [0u8; 16];
    mapped[10] = 0xff;
    mapped[11] = 0xff;
    mapped[12..].copy_from_slice(&[127, 0, 0, 1]);
    let mut other = [0u8; 16];
    other[0] = 0xfe;
    other[1] = 0x80;
    other[15] = 1;
    let text = [
        HEADER.to_owned(),
        row(&v4([0, 0, 0, 0], 3000), "0A"),
        row(&v4([127, 0, 0, 1], 8080), "0A"),
        row(&v4([127, 0, 0, 5], 8081), "0A"),
        row(&v4([172, 17, 0, 1], 9000), "0A"),
        row(&v4([0, 0, 0, 0], 9001), "01"),
        row(&v6([0; 16], 4000), "0A"),
        row(&v6(v6_loopback, 4001), "0A"),
        row(&v6(mapped, 4002), "0A"),
        row(&v6(other, 4003), "0A"),
        row(&v4([0, 0, 0, 0], 0), "0A"),
    ]
    .concat();
    let mut ports = parse_table(&text);
    ports.sort_unstable();
    assert_eq!(ports, [3000, 4000, 4001, 4002, 8080, 8081]);
}

#[test]
fn rows_the_kernel_would_not_write_are_skipped() {
    let text = [
        HEADER,
        "garbage\n",
        "0: 0100007F 00000000:0000 0A\n",
        "0: 0100007F:ZZZZ 00000000:0000 0A x\n",
        "0: 0100007F:0050 00000000:0000\n",
        "0: 0100:0050 00000000:0000 0A x\n",
        "0: 0100007G:0050 00000000:0000 0A x\n",
        "0: 000000000000000000000000000000ZZ:0050 00000000:0000 0A x\n",
        "\u{0}\u{1}\n",
    ]
    .concat();
    assert_eq!(parse_table(&text), Vec::<u16>::new());
    assert_eq!(parse_table(""), Vec::<u16>::new());
}

#[test]
fn the_agents_own_ports_are_left_out_and_a_missing_ipv6_table_is_fine() {
    let tmp = tempfile::Builder::new()
        .prefix("puddle-ports-read-")
        .tempdir()
        .unwrap();
    let dir = tmp.path().to_path_buf();
    std::fs::write(
        dir.join("tcp"),
        [
            HEADER.to_owned(),
            row(&v4([0; 4], 3128), "0A"),
            row(&v4([0; 4], 3000), "0A"),
        ]
        .concat(),
    )
    .unwrap();
    assert_eq!(read(&dir, &[3128]).unwrap(), BTreeSet::from([3000]));
    std::fs::remove_dir_all(&dir).unwrap();
    let err = read(&dir, &[]).unwrap_err();
    assert!(err.to_string().contains("puddle-ports-read"), "{err}");
}

#[tokio::test(start_paused = true)]
async fn the_set_is_sent_at_start_and_again_only_when_it_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().to_path_buf();
    let write = |ports: &[u16]| {
        let rows: String = ports.iter().map(|p| row(&v4([0; 4], *p), "0A")).collect();
        std::fs::write(dir.join("tcp"), format!("{HEADER}{rows}")).unwrap();
    };
    write(&[]);
    let (tx, mut rx) = mpsc::channel(8);
    let task = tokio::spawn(watch(
        PortWatch {
            dir: dir.clone(),
            poll: Duration::from_millis(100),
        },
        vec![],
        tx,
    ));
    assert_eq!(rx.recv().await, Some(AgentMessage::ports([])));
    // Nothing changes: nothing is sent.
    no_event_for(
        "a report while nothing changes",
        Duration::from_millis(500),
        &mut rx,
    )
    .await;
    write(&[8080, 3000]);
    assert_eq!(rx.recv().await, Some(AgentMessage::ports([3000, 8080])));
    write(&[3000]);
    assert_eq!(rx.recv().await, Some(AgentMessage::ports([3000])));
    // Unreadable tables say nothing and the watch carries on when they come back.
    std::fs::remove_file(dir.join("tcp")).unwrap();
    no_event_for(
        "a report while the table is unreadable",
        Duration::from_millis(300),
        &mut rx,
    )
    .await;
    write(&[3000, 5000]);
    assert_eq!(rx.recv().await, Some(AgentMessage::ports([3000, 5000])));
    drop(rx);
    task.await.unwrap();
}
