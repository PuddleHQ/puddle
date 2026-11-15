// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

/// The text of `/proc/net/route`: a header and one tab-separated row per route.
fn table() -> String {
    let rows = [
        [
            "eth0", "00000000", "0500A8C0", "0003", "0", "0", "100", "00000000",
        ],
        [
            "eth1", "00000000", "0100A8C0", "0003", "0", "0", "50", "00000000",
        ],
        [
            "wlan0", "00000000", "0100A8C0", "0002", "0", "0", "10", "00000000",
        ],
        [
            "docker0", "000011AC", "00000000", "0001", "0", "0", "0", "0000FFFF",
        ],
    ];
    let mut text =
        "Iface Destination Gateway Flags RefCnt Use Metric Mask MTU Window IRTT\n".to_owned();
    for row in rows {
        text.push_str(&row.join("\t"));
        text.push('\n');
    }
    text
}

#[test]
fn the_default_route_is_the_lowest_metric_one_that_is_up() {
    assert_eq!(default_route_interface(&table()).as_deref(), Some("eth1"));
}

#[test]
fn no_default_route_is_none_even_with_other_routes() {
    let all = table();
    let table = all
        .lines()
        .filter(|l| l.starts_with("Iface") || l.starts_with("docker0"))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(default_route_interface(&table), None);
    assert_eq!(default_route_interface(""), None);
    assert_eq!(default_route_interface("Iface\nbroken line"), None);
}

#[test]
fn an_interface_message_puts_the_index_and_flags_where_the_kernel_reads_them() {
    let m = ifinfo(5, IFF_UP, IFF_UP);
    assert_eq!(i32::from_ne_bytes(m[4..8].try_into().unwrap()), 5);
    assert_eq!(u32::from_ne_bytes(m[8..12].try_into().unwrap()), 1);
    assert_eq!(u32::from_ne_bytes(m[12..16].try_into().unwrap()), 1);
}
