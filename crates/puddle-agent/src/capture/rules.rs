// SPDX-License-Identifier: GPL-3.0-or-later
//! The capture rules as data: which packets are redirected to the agent, which fail at once.
//!
//! [`Plan::capture`] is the whole rule set. [`nft`](super::nft) turns it into kernel messages and
//! `Display` prints it in `nft` syntax for the log, so what is installed can be read without a
//! firewall tool. Everything here is pure, so the rules are unit-tested without a kernel.
//!
//! The table is `inet puddle`; its hooks run before Docker's (priority -110 for the nat chains,
//! which is ahead of `dstnat`), and Docker's iptables-nft rules live in other tables, so the
//! two coexist whichever starts first.

use std::fmt;
use std::net::Ipv4Addr;

/// The table the rules live in.
pub const TABLE: &str = "puddle";

/// `198.18.0.0/15`: the stand-in range the stub DNS hands out.
pub const STAND_IN_NET: Ipv4Net = Ipv4Net {
    addr: Ipv4Addr::new(198, 18, 0, 0),
    prefix: 15,
};

/// An IPv4 network.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ipv4Net {
    /// The network address.
    pub addr: Ipv4Addr,
    /// Prefix length, 0 to 32.
    pub prefix: u8,
}

impl Ipv4Net {
    /// The netmask as four bytes.
    #[must_use]
    pub fn mask(self) -> [u8; 4] {
        let bits = u32::MAX
            .checked_shl(32u32.saturating_sub(u32::from(self.prefix)))
            .unwrap_or(0);
        bits.to_be_bytes()
    }

    /// The network address with the host bits cleared.
    #[must_use]
    pub fn network(self) -> [u8; 4] {
        let mask = self.mask();
        let mut out = self.addr.octets();
        for (byte, m) in out.iter_mut().zip(mask) {
            *byte &= m;
        }
        out
    }
}

impl fmt::Display for Ipv4Net {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.prefix >= 32 {
            write!(f, "{}", self.addr)
        } else {
            write!(f, "{}/{}", Ipv4Addr::from(self.network()), self.prefix)
        }
    }
}

/// What the rules are built from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// The stub DNS address: DNS goes there, and nothing else is rewritten for it.
    pub stub: Ipv4Addr,
    /// The stub DNS port.
    pub stub_port: u16,
    /// The stand-in range.
    pub stand_ins: Ipv4Net,
    /// The port of the transparent listener.
    pub listen_port: u16,
    /// The interface the default route leaves by: what is routed there is outbound.
    pub wan: String,
    /// The netfilter log group refused UDP is reported on; `None` leaves the logging out.
    pub log_group: Option<u16>,
}

/// The kind of chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `type nat`.
    Nat,
    /// `type filter`.
    Filter,
}

/// Where a chain hooks in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hook {
    /// Packets arriving, before routing (containers' traffic).
    Prerouting,
    /// Packets being forwarded.
    Forward,
    /// Packets the guest's own processes send.
    Output,
}

impl Hook {
    /// `NF_INET_*`.
    #[must_use]
    pub fn number(self) -> u32 {
        match self {
            Self::Prerouting => 0,
            Self::Forward => 2,
            Self::Output => 3,
        }
    }
}

/// A transport protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum L4 {
    /// TCP.
    Tcp,
    /// UDP.
    Udp,
}

impl L4 {
    /// The IP protocol number.
    #[must_use]
    pub fn number(self) -> u8 {
        match self {
            Self::Tcp => 6,
            Self::Udp => 17,
        }
    }
}

/// One condition of a rule; all of a rule's conditions must hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Match {
    /// The packet is IPv4 (a guard the address matches below need in an `inet` table).
    Ipv4,
    /// The transport protocol.
    L4(L4),
    /// The transport destination port.
    DstPort(u16),
    /// The IPv4 destination address is in the network.
    Daddr(Ipv4Net),
    /// The interface the packet leaves by.
    OutIf(String),
    /// The interface the packet arrived on.
    InIf(String),
    /// The routing table would send the destination out of this interface.
    FibOutIf(String),
    /// Conntrack says the connection was destination-NATed (by us, or by Docker's port publishing).
    CtDnat,
}

/// What a rule does; the last one is the verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stmt {
    /// Passes within a rate, matches nothing beyond it.
    Limit {
        /// Packets per second.
        per_second: u32,
        /// Burst size.
        burst: u32,
    },
    /// Copies the packet to a netfilter log group.
    Log {
        /// The group.
        group: u16,
        /// Bytes of the packet to copy.
        snaplen: u32,
    },
    /// Stops this chain's evaluation as accepted.
    Accept,
    /// Leaves the chain without deciding.
    Return,
    /// Redirects to a port on this host.
    Redirect(u16),
    /// Rewrites the destination.
    Dnat(Ipv4Addr, u16),
    /// Refuses at once.
    Reject(Reject),
}

/// How a refusal looks to the sender.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reject {
    /// A TCP reset.
    TcpReset,
    /// ICMP port unreachable (`ECONNREFUSED` for UDP).
    PortUnreachable,
}

/// One rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    /// All must hold.
    pub matches: Vec<Match>,
    /// Run in order when they do.
    pub then: Vec<Stmt>,
}

/// One chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chain {
    /// Its name.
    pub name: &'static str,
    /// Nat or filter.
    pub kind: Kind,
    /// Where it hooks in.
    pub hook: Hook,
    /// Priority within the hook.
    pub priority: i32,
    /// The rules in order.
    pub rules: Vec<Rule>,
}

/// The whole rule set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// The chains.
    pub chains: Vec<Chain>,
}

/// UDP packets per second one chain reports; the rest are refused without a report.
const LOG_RATE: u32 = 20;
/// Bytes of a refused packet the log keeps: enough for the IPv6 header and the ports.
const LOG_SNAPLEN: u32 = 64;

fn rule(matches: Vec<Match>, then: Vec<Stmt>) -> Rule {
    Rule { matches, then }
}

impl Plan {
    /// The capture rules.
    ///
    /// - DNS to any address but loopback goes to the stub.
    /// - TCP to a stand-in, and TCP to anything the routing table sends out of `wan`, is redirected
    ///   to the listener, which sends it to the proxy by name or by address.
    /// - Loopback, the guest's own addresses and the Docker bridges are not outbound: the routing
    ///   table keeps them off `wan`, so no rule names them.
    /// - What is still left, UDP other than DNS, IPv6 and ICMP out of `wan`, and anything to a
    ///   stand-in that was not redirected, is refused at once. Refused UDP is also logged to a
    ///   netfilter group, rate-limited, so the agent can tell the host.
    #[must_use]
    pub fn capture(s: &Settings) -> Self {
        Self {
            chains: vec![
                nat_chain(s, "nat_out", Hook::Output),
                nat_chain(s, "nat_pre", Hook::Prerouting),
                refuse_chain(s, "filter_out", Hook::Output),
                refuse_chain(s, "filter_fwd", Hook::Forward),
            ],
        }
    }

    /// Chain name to rule count, in chain order.
    #[must_use]
    pub fn shape(&self) -> Vec<(&'static str, usize)> {
        self.chains
            .iter()
            .map(|c| (c.name, c.rules.len()))
            .collect()
    }
}

fn host(addr: Ipv4Addr) -> Match {
    Match::Daddr(Ipv4Net { addr, prefix: 32 })
}

/// DNS to any address is the stub's.
fn dns(s: &Settings, l4: L4) -> Rule {
    rule(
        vec![Match::Ipv4, Match::L4(l4), Match::DstPort(53)],
        vec![Stmt::Dnat(s.stub, s.stub_port)],
    )
}

/// The nat chain of one hook. At output it also leaves loopback alone and redirects what the
/// routing table sends out of the outbound interface; in prerouting (containers' traffic) it asks
/// the routing table the same question with `fib`, and leaves what arrived on the outbound
/// interface alone.
fn nat_chain(s: &Settings, name: &'static str, hook: Hook) -> Chain {
    let to_listener = vec![Stmt::Redirect(s.listen_port)];
    let mut rules = Vec::new();
    match hook {
        Hook::Output => {}
        Hook::Prerouting | Hook::Forward => {
            rules.push(rule(vec![Match::InIf(s.wan.clone())], vec![Stmt::Return]));
        }
    }
    rules.push(rule(vec![Match::Ipv4, host(s.stub)], vec![Stmt::Return]));
    if hook == Hook::Output {
        let loopback = Ipv4Net {
            addr: Ipv4Addr::new(127, 0, 0, 0),
            prefix: 8,
        };
        rules.push(rule(
            vec![Match::Ipv4, Match::Daddr(loopback)],
            vec![Stmt::Return],
        ));
    }
    rules.push(dns(s, L4::Udp));
    rules.push(dns(s, L4::Tcp));
    rules.push(rule(
        vec![Match::Ipv4, Match::Daddr(s.stand_ins), Match::L4(L4::Tcp)],
        to_listener.clone(),
    ));
    let outbound = if hook == Hook::Output {
        Match::OutIf(s.wan.clone())
    } else {
        Match::FibOutIf(s.wan.clone())
    };
    rules.push(rule(
        vec![Match::Ipv4, outbound, Match::L4(L4::Tcp)],
        to_listener,
    ));
    Chain {
        name,
        kind: Kind::Nat,
        hook,
        priority: -110,
        rules,
    }
}

/// A filter chain that lets captured flows pass and fails everything else outbound at once,
/// logging refused UDP first when there is a log group.
fn refuse_chain(s: &Settings, name: &'static str, hook: Hook) -> Chain {
    let log_udp = |matches: Vec<Match>| {
        s.log_group.map(|group| {
            let mut matches = matches;
            matches.push(Match::L4(L4::Udp));
            rule(
                matches,
                vec![
                    Stmt::Limit {
                        per_second: LOG_RATE,
                        burst: LOG_RATE,
                    },
                    Stmt::Log {
                        group,
                        snaplen: LOG_SNAPLEN,
                    },
                ],
            )
        })
    };
    let stand_ins = Match::Daddr(s.stand_ins);
    let wan = Match::OutIf(s.wan.clone());
    Chain {
        name,
        kind: Kind::Filter,
        hook,
        priority: 0,
        rules: [
            Some(rule(vec![Match::CtDnat], vec![Stmt::Accept])),
            Some(rule(vec![Match::Ipv4, host(s.stub)], vec![Stmt::Accept])),
            log_udp(vec![Match::Ipv4, stand_ins.clone()]),
            Some(rule(
                vec![Match::Ipv4, stand_ins],
                vec![Stmt::Reject(Reject::PortUnreachable)],
            )),
            log_udp(vec![wan.clone()]),
            Some(rule(
                vec![wan.clone(), Match::L4(L4::Tcp)],
                vec![Stmt::Reject(Reject::TcpReset)],
            )),
            Some(rule(vec![wan], vec![Stmt::Reject(Reject::PortUnreachable)])),
        ]
        .into_iter()
        .flatten()
        .collect(),
    }
}

impl fmt::Display for Match {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ipv4 => f.write_str("meta nfproto ipv4"),
            Self::L4(L4::Tcp) => f.write_str("meta l4proto tcp"),
            Self::L4(L4::Udp) => f.write_str("meta l4proto udp"),
            Self::DstPort(port) => write!(f, "th dport {port}"),
            Self::Daddr(net) => write!(f, "ip daddr {net}"),
            Self::OutIf(name) => write!(f, "oifname \"{name}\""),
            Self::InIf(name) => write!(f, "iifname \"{name}\""),
            Self::FibOutIf(name) => write!(f, "fib daddr oifname \"{name}\""),
            Self::CtDnat => f.write_str("ct status dnat"),
        }
    }
}

impl fmt::Display for Stmt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Limit { per_second, burst } => {
                write!(f, "limit rate {per_second}/second burst {burst} packets")
            }
            Self::Log { group, snaplen } => write!(f, "log group {group} snaplen {snaplen}"),
            Self::Accept => f.write_str("accept"),
            Self::Return => f.write_str("return"),
            Self::Redirect(port) => write!(f, "redirect to :{port}"),
            Self::Dnat(addr, port) => write!(f, "dnat ip to {addr}:{port}"),
            Self::Reject(Reject::TcpReset) => f.write_str("reject with tcp reset"),
            Self::Reject(Reject::PortUnreachable) => {
                f.write_str("reject with icmpx type port-unreachable")
            }
        }
    }
}

impl fmt::Display for Rule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let words: Vec<String> = self
            .matches
            .iter()
            .map(ToString::to_string)
            .chain(self.then.iter().map(ToString::to_string))
            .collect();
        f.write_str(&words.join(" "))
    }
}

impl fmt::Display for Plan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "table inet {TABLE} {{")?;
        for chain in &self.chains {
            let kind = match chain.kind {
                Kind::Nat => "nat",
                Kind::Filter => "filter",
            };
            let hook = match chain.hook {
                Hook::Prerouting => "prerouting",
                Hook::Forward => "forward",
                Hook::Output => "output",
            };
            let header = format!(
                "  chain {} {{ type {kind} hook {hook} priority {}; }}",
                chain.name, chain.priority
            );
            writeln!(f, "{header}")?;
            for rule in &chain.rules {
                writeln!(f, "    {} {rule}", chain.name)?;
            }
        }
        writeln!(f, "}}")
    }
}

#[cfg(test)]
mod tests;
