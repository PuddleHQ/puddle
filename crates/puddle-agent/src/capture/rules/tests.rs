// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn settings() -> Settings {
    Settings {
        stub: Ipv4Addr::new(198, 18, 0, 1),
        stub_port: 53,
        stand_ins: STAND_IN_NET,
        listen_port: 3129,
        wan: "eth0".to_owned(),
        log_group: Some(17),
    }
}

fn chain<'a>(plan: &'a Plan, name: &str) -> &'a Chain {
    plan.chains.iter().find(|c| c.name == name).unwrap()
}

#[test]
fn a_network_prints_and_masks_the_way_nft_does() {
    assert_eq!(STAND_IN_NET.to_string(), "198.18.0.0/15");
    assert_eq!(STAND_IN_NET.mask(), [255, 254, 0, 0]);
    let host = Ipv4Net {
        addr: Ipv4Addr::new(198, 18, 0, 1),
        prefix: 32,
    };
    assert_eq!(host.to_string(), "198.18.0.1");
    assert_eq!(host.mask(), [255; 4]);
    let odd = Ipv4Net {
        addr: Ipv4Addr::new(10, 1, 2, 3),
        prefix: 8,
    };
    assert_eq!(odd.network(), [10, 0, 0, 0]);
    assert_eq!(
        Ipv4Net {
            addr: Ipv4Addr::UNSPECIFIED,
            prefix: 0
        }
        .mask(),
        [0; 4]
    );
}

#[test]
fn the_four_chains_hook_where_capture_needs_them() {
    let plan = Plan::capture(&settings());
    assert_eq!(
        plan.chains
            .iter()
            .map(|c| (c.name, c.hook, c.priority))
            .collect::<Vec<_>>(),
        vec![
            ("nat_out", Hook::Output, -110),
            ("nat_pre", Hook::Prerouting, -110),
            ("filter_out", Hook::Output, 0),
            ("filter_fwd", Hook::Forward, 0),
        ]
    );
    assert_eq!(
        plan.shape(),
        vec![
            ("nat_out", 6),
            ("nat_pre", 6),
            ("filter_out", 7),
            ("filter_fwd", 7)
        ]
    );
}

#[test]
fn loopback_and_the_stub_are_left_alone_before_anything_is_rewritten() {
    let out = chain(&Plan::capture(&settings()), "nat_out").clone();
    assert_eq!(
        out.rules[0].to_string(),
        "meta nfproto ipv4 ip daddr 198.18.0.1 return"
    );
    assert_eq!(
        out.rules[1].to_string(),
        "meta nfproto ipv4 ip daddr 127.0.0.0/8 return"
    );
    assert!(
        out.rules[2]
            .to_string()
            .contains("th dport 53 dnat ip to 198.18.0.1:53")
    );
}

#[test]
fn tcp_to_a_stand_in_or_out_of_the_wan_goes_to_the_listener() {
    let plan = Plan::capture(&settings());
    let text = plan.to_string();
    assert!(text.contains("ip daddr 198.18.0.0/15 meta l4proto tcp redirect to :3129"));
    assert!(text.contains("oifname \"eth0\" meta l4proto tcp redirect to :3129"));
    assert!(text.contains("fib daddr oifname \"eth0\" meta l4proto tcp redirect to :3129"));
    // Traffic that arrived on the wan is not ours to redirect.
    assert_eq!(
        chain(&plan, "nat_pre").rules[0].to_string(),
        "iifname \"eth0\" return"
    );
}

#[test]
fn captured_flows_pass_the_filter_first_and_everything_else_out_of_the_wan_fails() {
    for name in ["filter_out", "filter_fwd"] {
        let plan = Plan::capture(&settings());
        let c = chain(&plan, name);
        assert_eq!(c.rules[0].to_string(), "ct status dnat accept", "{name}");
        assert_eq!(
            c.rules[1].to_string(),
            "meta nfproto ipv4 ip daddr 198.18.0.1 accept"
        );
        let last = c.rules.last().unwrap();
        assert_eq!(
            last.to_string(),
            "oifname \"eth0\" reject with icmpx type port-unreachable"
        );
        // The reset for TCP comes before the catch-all, so a refused connect is instant.
        assert!(c.rules.iter().any(
                |r| r.to_string() == "oifname \"eth0\" meta l4proto tcp reject with tcp reset"
            ));
    }
}

#[test]
fn without_a_log_group_nothing_is_logged_and_the_rest_is_the_same() {
    let mut quiet = settings();
    quiet.log_group = None;
    let plan = Plan::capture(&quiet);
    assert!(!plan.to_string().contains("log group"));
    assert_eq!(
        plan.shape(),
        vec![
            ("nat_out", 6),
            ("nat_pre", 6),
            ("filter_out", 5),
            ("filter_fwd", 5)
        ]
    );
}

#[test]
fn refused_udp_is_logged_before_it_is_refused_and_the_log_is_rate_limited() {
    let plan = Plan::capture(&settings());
    let c = chain(&plan, "filter_out");
    let texts: Vec<String> = c.rules.iter().map(ToString::to_string).collect();
    let log_at = |needle: &str| texts.iter().position(|t| t.contains(needle)).unwrap();
    let wan_log = texts
        .iter()
        .position(|t| t.starts_with("oifname \"eth0\" meta l4proto udp limit rate 20/second"))
        .unwrap();
    assert!(texts[wan_log].ends_with("log group 17 snaplen 64"));
    assert!(wan_log < log_at("reject with tcp reset"));
    let stand_in_log = texts
        .iter()
        .position(|t| {
            t.contains("meta l4proto udp") && t.contains("198.18.0.0/15") && t.contains("log group")
        })
        .unwrap();
    let stand_in_reject = texts
        .iter()
        .position(|t| t.ends_with("ip daddr 198.18.0.0/15 reject with icmpx type port-unreachable"))
        .unwrap();
    assert!(stand_in_log < stand_in_reject);
}

#[test]
fn the_listing_names_the_table_and_every_chain() {
    let text = Plan::capture(&settings()).to_string();
    assert!(text.starts_with("table inet puddle {"));
    for name in ["nat_out", "nat_pre", "filter_out", "filter_fwd"] {
        assert!(text.contains(&format!("chain {name} {{ type ")), "{name}");
    }
    assert!(text.contains("type nat hook output priority -110"));
    assert!(text.contains("type filter hook forward priority 0"));
    assert!(text.contains("type nat hook prerouting priority -110"));
}

#[test]
fn hooks_and_protocols_have_their_kernel_numbers() {
    assert_eq!(
        [
            Hook::Prerouting.number(),
            Hook::Forward.number(),
            Hook::Output.number()
        ],
        [0, 2, 3]
    );
    assert_eq!([L4::Tcp.number(), L4::Udp.number()], [6, 17]);
}
