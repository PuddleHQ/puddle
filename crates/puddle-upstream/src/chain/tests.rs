// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;

#[test]
fn the_families_alternate_from_the_first_address_on_each_in_its_own_order() {
    let a: Vec<SocketAddr> = ["[::1]:1", "[::2]:1", "[::3]:1", "10.0.0.1:1", "10.0.0.2:1"]
        .iter()
        .map(|s| s.parse().unwrap())
        .collect();
    let order: Vec<String> = interleaved(&a).iter().map(ToString::to_string).collect();
    assert_eq!(
        order,
        ["[::1]:1", "10.0.0.1:1", "[::2]:1", "10.0.0.2:1", "[::3]:1"]
    );
    let b: Vec<SocketAddr> = ["10.0.0.1:1", "10.0.0.2:1", "10.0.0.3:1", "[::1]:1"]
        .iter()
        .map(|s| s.parse().unwrap())
        .collect();
    let order: Vec<String> = interleaved(&b).iter().map(ToString::to_string).collect();
    assert_eq!(order, ["10.0.0.1:1", "[::1]:1", "10.0.0.2:1", "10.0.0.3:1"]);
    assert_eq!(interleaved(&[]), Vec::<SocketAddr>::new());
}
