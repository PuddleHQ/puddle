// SPDX-License-Identifier: GPL-3.0-or-later
//! Installs a [`Plan`] into the kernel's `nf_tables` over netlink, and reads back what is there.
//!
//! No `nft` binary and no C library: the messages are written here (`Writer`). The attribute
//! numbers are the kernel's (`linux/netfilter/nf_tables.h`); the integration tests install the plan
//! in a network namespace and read it back with the real `nft`, which is the check that they match.
//!
//! Installing is idempotent: one batch makes the table, deletes it (with whatever a previous
//! run or a guest left in it) and makes it again, so a rerun converges and the kernel applies the
//! whole batch or none of it.

use std::io;

use super::netlink::{
    F_ACK, F_APPEND, F_CREATE, F_DUMP, F_REQUEST, Message, Socket, Writer, attrs, messages,
};
use super::rules::{Chain, Kind, Match, Plan, Reject, Rule, Stmt, TABLE};

/// `NFNL_SUBSYS_NFTABLES`.
const SUBSYS: u16 = 10;
/// `NFNL_MSG_BATCH_BEGIN` and `_END`.
const BATCH_BEGIN: u16 = 0x10;
const BATCH_END: u16 = 0x11;
/// `NFPROTO_INET`, `NFPROTO_IPV4`.
const INET: u8 = 1;
const IPV4: u32 = 2;

const NEWTABLE: u16 = 0;
const GETTABLE: u16 = 1;
const DELTABLE: u16 = 2;
const NEWCHAIN: u16 = 3;
const GETCHAIN: u16 = 4;
const NEWRULE: u16 = 6;
const GETRULE: u16 = 7;

/// `NFT_REG_VERDICT`, `NFT_REG_1`, `NFT_REG_2`.
const VERDICT: u32 = 0;
const REG1: u32 = 1;
const REG2: u32 = 2;
/// `NFT_CMP_EQ`, `NFT_CMP_NEQ`.
const EQ: u32 = 0;
const NEQ: u32 = 1;
/// Verdict codes: `NF_ACCEPT`, and `NFT_RETURN` (-5).
const NF_ACCEPT: u32 = 1;
const NFT_RETURN: u32 = 0xFFFF_FFFB;
/// `IPS_DST_NAT`.
const IPS_DST_NAT: u32 = 1 << 5;
/// Length of an interface name field.
const IFNAMSIZ: usize = 16;

fn kind(msg: u16) -> u16 {
    (SUBSYS << 8) | msg
}

/// One expression: its name and a body filler.
fn expr(w: &mut Writer, name: &str, fill: impl FnOnce(&mut Writer)) {
    w.nested(1, |w| {
        w.string(1, name);
        w.nested(2, fill);
    });
}

/// A `NFTA_DATA_VALUE` inside the nested attribute `ty`.
fn data(w: &mut Writer, ty: u16, value: &[u8]) {
    w.nested(ty, |w| w.attr(1, value));
}

fn ifname(name: &str) -> [u8; IFNAMSIZ] {
    let mut out = [0u8; IFNAMSIZ];
    for (slot, byte) in out.iter_mut().zip(name.bytes().take(IFNAMSIZ - 1)) {
        *slot = byte;
    }
    out
}

fn meta(w: &mut Writer, key: u32) {
    expr(w, "meta", |w| {
        w.be32(1, REG1);
        w.be32(2, key);
    });
}

fn cmp(w: &mut Writer, op: u32, value: &[u8]) {
    expr(w, "cmp", |w| {
        w.be32(1, REG1);
        w.be32(2, op);
        data(w, 3, value);
    });
}

fn payload(w: &mut Writer, base: u32, offset: u32, len: u32) {
    expr(w, "payload", |w| {
        w.be32(1, REG1);
        w.be32(2, base);
        w.be32(3, offset);
        w.be32(4, len);
    });
}

fn bitwise(w: &mut Writer, mask: &[u8]) {
    let len = u32::try_from(mask.len()).unwrap_or(0);
    expr(w, "bitwise", |w| {
        w.be32(1, REG1);
        w.be32(2, REG1);
        w.be32(3, len);
        data(w, 4, mask);
        data(w, 5, &vec![0; mask.len()]);
    });
}

fn immediate_value(w: &mut Writer, reg: u32, value: &[u8]) {
    expr(w, "immediate", |w| {
        w.be32(1, reg);
        data(w, 2, value);
    });
}

fn immediate_verdict(w: &mut Writer, code: u32) {
    expr(w, "immediate", |w| {
        w.be32(1, VERDICT);
        w.nested(2, |w| w.nested(2, |w| w.be32(1, code)));
    });
}

fn encode_match(w: &mut Writer, m: &Match) {
    match m {
        Match::Ipv4 => {
            meta(w, 15);
            cmp(w, EQ, &[2]);
        }
        Match::L4(l4) => {
            meta(w, 16);
            cmp(w, EQ, &[l4.number()]);
        }
        Match::DstPort(port) => {
            payload(w, 2, 2, 2);
            cmp(w, EQ, &port.to_be_bytes());
        }
        Match::Daddr(net) => {
            payload(w, 1, 16, 4);
            if net.prefix < 32 {
                bitwise(w, &net.mask());
            }
            cmp(w, EQ, &net.network());
        }
        Match::OutIf(name) => {
            meta(w, 7);
            cmp(w, EQ, &ifname(name));
        }
        Match::InIf(name) => {
            meta(w, 6);
            cmp(w, EQ, &ifname(name));
        }
        Match::FibOutIf(name) => {
            expr(w, "fib", |w| {
                w.be32(1, REG1);
                w.be32(2, 2);
                w.be32(3, 2);
            });
            cmp(w, EQ, &ifname(name));
        }
        Match::CtDnat => {
            expr(w, "ct", |w| {
                w.be32(1, REG1);
                w.be32(2, 2);
            });
            bitwise(w, &IPS_DST_NAT.to_ne_bytes());
            cmp(w, NEQ, &[0; 4]);
        }
    }
}

fn encode_stmt(w: &mut Writer, s: &Stmt) {
    match s {
        Stmt::Limit { per_second, burst } => expr(w, "limit", |w| {
            w.attr(1, &u64::from(*per_second).to_be_bytes());
            w.attr(2, &1u64.to_be_bytes());
            w.be32(3, *burst);
            w.be32(4, 0);
            w.be32(5, 0);
        }),
        Stmt::Log { group, snaplen } => expr(w, "log", |w| {
            w.attr(1, &group.to_be_bytes());
            w.be32(3, *snaplen);
        }),
        Stmt::Accept => immediate_verdict(w, NF_ACCEPT),
        Stmt::Return => immediate_verdict(w, NFT_RETURN),
        Stmt::Redirect(port) => {
            immediate_value(w, REG1, &port.to_be_bytes());
            expr(w, "redir", |w| w.be32(1, REG1));
        }
        Stmt::Dnat(addr, port) => {
            immediate_value(w, REG1, &addr.octets());
            immediate_value(w, REG2, &port.to_be_bytes());
            expr(w, "nat", |w| {
                w.be32(1, 1);
                w.be32(2, IPV4);
                w.be32(3, REG1);
                w.be32(5, REG2);
            });
        }
        Stmt::Reject(Reject::TcpReset) => expr(w, "reject", |w| w.be32(1, 1)),
        Stmt::Reject(Reject::PortUnreachable) => expr(w, "reject", |w| {
            w.be32(1, 2);
            w.attr(2, &[1]);
        }),
    }
}

fn encode_rule(w: &mut Writer, seq: u32, chain: &Chain, rule: &Rule) {
    let at = w.begin(kind(NEWRULE), F_REQUEST | F_ACK | F_CREATE | F_APPEND, seq);
    w.nfgen(INET, 0);
    w.string(1, TABLE);
    w.string(2, chain.name);
    w.nested(4, |w| {
        for m in &rule.matches {
            encode_match(w, m);
        }
        for s in &rule.then {
            encode_stmt(w, s);
        }
    });
    w.end(at);
}

fn encode_chain(w: &mut Writer, seq: u32, chain: &Chain) {
    let at = w.begin(kind(NEWCHAIN), F_REQUEST | F_ACK | F_CREATE, seq);
    w.nfgen(INET, 0);
    w.string(1, TABLE);
    w.string(3, chain.name);
    w.nested(4, |w| {
        w.be32(1, chain.hook.number());
        w.be32(2, u32::from_ne_bytes(chain.priority.to_ne_bytes()));
    });
    w.string(
        7,
        match chain.kind {
            Kind::Nat => "nat",
            Kind::Filter => "filter",
        },
    );
    w.be32(5, NF_ACCEPT);
    w.end(at);
}

fn encode_table(w: &mut Writer, seq: u32, msg: u16, flags: u16) {
    let at = w.begin(kind(msg), F_REQUEST | F_ACK | flags, seq);
    w.nfgen(INET, 0);
    w.string(1, TABLE);
    w.end(at);
}

/// The batch that replaces the table with `plan`, and how many acknowledgements it earns.
fn encode(plan: &Plan) -> (Vec<u8>, usize) {
    let mut w = Writer::default();
    let begin = w.begin(BATCH_BEGIN, F_REQUEST, 0);
    w.nfgen(0, SUBSYS);
    w.end(begin);
    let mut seq = 1;
    // Make sure it exists, delete it, make it: the delete of a missing table would fail the batch.
    encode_table(&mut w, seq, NEWTABLE, F_CREATE);
    seq += 1;
    encode_table(&mut w, seq, DELTABLE, 0);
    seq += 1;
    encode_table(&mut w, seq, NEWTABLE, F_CREATE);
    seq += 1;
    for chain in &plan.chains {
        encode_chain(&mut w, seq, chain);
        seq += 1;
    }
    for chain in &plan.chains {
        for rule in &chain.rules {
            encode_rule(&mut w, seq, chain, rule);
            seq += 1;
        }
    }
    let end = w.begin(BATCH_END, F_REQUEST, seq);
    w.nfgen(0, SUBSYS);
    w.end(end);
    (w.finish(), seq as usize - 1)
}

/// What the kernel holds for the table, in a form that compares equal when nothing changed.
/// Rule handles are left out: they differ between installs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprint {
    /// `(name, hook and priority and type)` per chain, sorted by name.
    chains: Vec<(String, Vec<u8>)>,
    /// `(chain, expressions)` per rule, in the kernel's order.
    rules: Vec<(String, Vec<u8>)>,
}

fn string(payload: &[u8]) -> String {
    let end = payload
        .iter()
        .position(|b| *b == 0)
        .unwrap_or(payload.len());
    String::from_utf8_lossy(payload.get(..end).unwrap_or_default()).into_owned()
}

fn dump_request(msg: u16, seq: u32) -> Vec<u8> {
    let mut w = Writer::default();
    let at = w.begin(kind(msg), F_REQUEST | F_DUMP, seq);
    w.nfgen(INET, 0);
    w.string(1, TABLE);
    w.end(at);
    w.finish()
}

/// Whether a dumped message is ours: `inet`, and this table.
fn ours(body: &[u8], table_attr: u16) -> Option<Vec<(u16, &[u8])>> {
    if *body.first()? != INET {
        return None;
    }
    let all = attrs(body.get(4..)?);
    let table = all.iter().find(|(ty, _)| *ty == table_attr)?;
    (string(table.1) == TABLE).then_some(all)
}

/// Replaces the table with `plan` in one batch and returns what the kernel then holds.
///
/// Blocking: call from `spawn_blocking`.
///
/// # Errors
///
/// The kernel refusing the batch (no `CAP_NET_ADMIN`, no `nf_tables`), which leaves the old table.
pub fn install(plan: &Plan) -> io::Result<Fingerprint> {
    let (batch, acks) = encode(plan);
    Socket::netfilter()?.transact(&batch, acks)?;
    read()?.ok_or_else(|| io::Error::other("the table is not there right after it was installed"))
}

/// The table as the kernel holds it, `None` when there is none.
///
/// Blocking: call from `spawn_blocking`.
///
/// # Errors
///
/// A netlink failure other than the table missing.
pub fn read() -> io::Result<Option<Fingerprint>> {
    let socket = Socket::netfilter()?;
    let mut get = Writer::default();
    let at = get.begin(kind(GETTABLE), F_REQUEST | F_ACK, 1);
    get.nfgen(INET, 0);
    get.string(1, TABLE);
    get.end(at);
    match socket.transact(&get.finish(), 1) {
        Ok(()) => {}
        Err(err) if err.raw_os_error() == Some(2) => return Ok(None),
        Err(err) => return Err(err),
    }
    let mut chains = Vec::new();
    for body in socket.dump(&dump_request(GETCHAIN, 2), kind(NEWCHAIN))? {
        let Some(all) = ours(&body, 1) else { continue };
        let mut identity = Vec::new();
        let mut name = String::new();
        for (ty, payload) in all {
            match ty {
                3 => name = string(payload),
                4 | 7 => {
                    identity.extend_from_slice(&ty.to_ne_bytes());
                    identity.extend_from_slice(payload);
                }
                _ => {}
            }
        }
        chains.push((name, identity));
    }
    chains.sort();
    let mut rules = Vec::new();
    for body in socket.dump(&dump_request(GETRULE, 3), kind(NEWRULE))? {
        let Some(all) = ours(&body, 1) else { continue };
        let chain = all.iter().find(|(ty, _)| *ty == 2).map(|(_, p)| string(p));
        let exprs = all.iter().find(|(ty, _)| *ty == 4).map(|(_, p)| p.to_vec());
        if let (Some(chain), Some(exprs)) = (chain, exprs) {
            rules.push((chain, exprs));
        }
    }
    Ok(Some(Fingerprint { chains, rules }))
}

/// Decodes one logged packet's destination (netfilter log message body): address, protocol and
/// destination port of an IPv4 or IPv6 packet, `None` for anything else.
#[must_use]
pub(crate) fn logged_destination(message: &Message<'_>) -> Option<(std::net::IpAddr, u8, u16)> {
    let all = attrs(message.body.get(4..)?);
    let packet = all
        .iter()
        .find(|(ty, _)| *ty == super::netlink::NFULA_PAYLOAD)?
        .1;
    destination(packet)
}

/// The destination address, protocol and port of a raw IP packet.
#[must_use]
pub fn destination(packet: &[u8]) -> Option<(std::net::IpAddr, u8, u16)> {
    let first = *packet.first()?;
    let (addr, proto, header): (std::net::IpAddr, u8, usize) = match first >> 4 {
        4 => {
            let octets: [u8; 4] = packet.get(16..20)?.try_into().ok()?;
            (
                octets.into(),
                *packet.get(9)?,
                usize::from(first & 0x0f) * 4,
            )
        }
        6 => {
            let octets: [u8; 16] = packet.get(24..40)?.try_into().ok()?;
            (octets.into(), *packet.get(6)?, 40)
        }
        _ => return None,
    };
    let port: [u8; 2] = packet.get(header + 2..header + 4)?.try_into().ok()?;
    Some((addr, proto, u16::from_be_bytes(port)))
}

/// Splits a receive buffer into the messages the log group delivered.
#[must_use]
pub fn logged(buf: &[u8]) -> Vec<(std::net::IpAddr, u8, u16)> {
    messages(buf)
        .iter()
        .filter(|m| super::netlink::is_packet(m))
        .filter_map(logged_destination)
        .collect()
}

#[cfg(test)]
mod tests {
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
                .filter(|(_, elem)| {
                    !string(attrs(elem).first().map_or(&[][..], |a| a.1)).is_empty()
                })
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
}
