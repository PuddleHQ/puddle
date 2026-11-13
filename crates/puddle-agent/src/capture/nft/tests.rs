// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use crate::capture::rules::{STAND_IN_NET, Settings};
use std::net::{IpAddr, Ipv4Addr};

fn plan() -> Plan {
    Plan::capture(&Settings {
        stub: Ipv4Addr::new(198, 18, 0, 1),
        stub_port: 53,
        stand_ins: STAND_IN_NET,
        listen_port: 3129,
        wan: "eth0".to_owned(),
        log_group: Some(17),
    })
}

/// A kernel whose table is deleted once the chain dump has been taken.
struct DeletedMidRead(std::cell::Cell<bool>);

impl Kernel for DeletedMidRead {
    fn table_exists(&self) -> io::Result<bool> {
        Ok(!self.0.get())
    }
    fn dump_chains(&self) -> io::Result<Vec<Vec<u8>>> {
        self.0.set(true);
        Ok(Vec::new())
    }
    fn dump_rules(&self) -> io::Result<Vec<Vec<u8>>> {
        Ok(Vec::new())
    }
}

#[test]
fn only_enoent_means_the_table_is_missing() {
    assert!(found(Ok(())).unwrap());
    assert!(!found(Err(io::Error::from_raw_os_error(2))).unwrap());
    let other = found(Err(io::Error::from_raw_os_error(1))).unwrap_err();
    assert_eq!(other.raw_os_error(), Some(1));
}

#[test]
fn a_table_deleted_between_the_check_and_the_dumps_reads_as_missing() {
    let read = read_from(&DeletedMidRead(std::cell::Cell::new(false))).unwrap();
    assert_eq!(read, None);
}

#[test]
fn the_batch_is_framed_and_counts_one_acknowledgement_per_message() {
    let plan = plan();
    let (batch, acks) = encode(&plan);
    let all = messages(&batch);
    let rules: usize = plan.chains.iter().map(|c| c.rules.len()).sum();
    assert_eq!(all.len(), 2 + 3 + plan.chains.len() + rules);
    assert_eq!(acks, 3 + plan.chains.len() + rules);
    assert_eq!(all.first().unwrap().ty, BATCH_BEGIN);
    assert_eq!(all.last().unwrap().ty, BATCH_END);
    let types: Vec<u16> = all.iter().skip(1).take(3).map(|m| m.ty).collect();
    assert_eq!(types, vec![kind(NEWTABLE), kind(DELTABLE), kind(NEWTABLE)]);
    let seqs: Vec<u32> = all.iter().map(|m| m.seq).collect();
    assert!(seqs.windows(2).all(|w| w[1] == w[0] + 1), "{seqs:?}");
}

#[test]
fn a_chain_carries_its_hook_priority_and_type() {
    let (batch, _) = encode(&plan());
    let nat_out = messages(&batch)
        .into_iter()
        .find(|m| m.ty == kind(NEWCHAIN))
        .unwrap();
    let all = attrs(&nat_out.body[4..]);
    let name = all.iter().find(|(t, _)| *t == 3).unwrap().1;
    assert_eq!(string(name), "nat_out");
    let hook = attrs(all.iter().find(|(t, _)| *t == 4).unwrap().1);
    assert_eq!(hook[0], (1, &3u32.to_be_bytes()[..]));
    assert_eq!(hook[1], (2, &(-110i32).cast_unsigned().to_be_bytes()[..]));
    assert_eq!(string(all.iter().find(|(t, _)| *t == 7).unwrap().1), "nat");
}

#[test]
fn a_daddr_match_masks_only_below_a_host_prefix() {
    let mut net = Writer::default();
    encode_match(&mut net, &Match::Daddr(STAND_IN_NET));
    let mut host = Writer::default();
    encode_match(
        &mut host,
        &Match::Daddr(crate::capture::rules::Ipv4Net {
            addr: Ipv4Addr::new(1, 2, 3, 4),
            prefix: 32,
        }),
    );
    let (net, host) = (net.finish(), host.finish());
    let count = |bytes: &[u8]| {
        attrs(bytes)
            .iter()
            .filter(|(_, elem)| !string(attrs(elem).first().map_or(&[][..], |a| a.1)).is_empty())
            .count()
    };
    assert_eq!(count(&net), 3, "payload, bitwise, cmp");
    assert_eq!(count(&host), 2, "payload, cmp");
}

#[test]
fn interface_names_are_padded_and_cut_to_the_kernels_field() {
    assert_eq!(&ifname("eth0")[..5], b"eth0\0");
    assert_eq!(ifname("a-very-long-interface-name")[IFNAMSIZ - 1], 0);
    assert_eq!(ifname("a-very-long-interface-name")[0], b'a');
}

#[test]
fn a_logged_packet_gives_its_destination_for_both_ip_versions() {
    let mut v4 = vec![0u8; 28];
    v4[0] = 0x45;
    v4[9] = 17;
    v4[16..20].copy_from_slice(&[93, 184, 216, 34]);
    v4[22..24].copy_from_slice(&443u16.to_be_bytes());
    assert_eq!(
        destination(&v4),
        Some((IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34)), 17, 443))
    );
    let mut v6 = vec![0u8; 48];
    v6[0] = 0x60;
    v6[6] = 17;
    v6[24..40].copy_from_slice(&std::net::Ipv6Addr::LOCALHOST.octets());
    v6[42..44].copy_from_slice(&53u16.to_be_bytes());
    assert_eq!(
        destination(&v6),
        Some((IpAddr::V6(std::net::Ipv6Addr::LOCALHOST), 17, 53))
    );
    assert_eq!(destination(&v4[..20]), None, "no room for the ports");
    assert_eq!(destination(&[]), None);
    assert_eq!(destination(&[0x10; 40]), None, "not IP");
}
