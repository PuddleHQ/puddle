// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn keys_are_the_stored_names() {
    let keys: Vec<_> = LocalCategory::ALL.iter().map(|c| c.key()).collect();
    assert_eq!(
        keys,
        [
            "loopback",
            "private",
            "link_local",
            "metadata",
            "special",
            "company_network"
        ]
    );
}

#[test]
fn serde_display_parse_and_audit_agree_with_the_key() {
    for c in LocalCategory::ALL {
        assert_eq!(
            serde_json::to_string(&c).unwrap(),
            format!("\"{}\"", c.key())
        );
        assert_eq!(
            serde_json::from_str::<LocalCategory>(&format!("\"{}\"", c.key())).unwrap(),
            c
        );
        assert_eq!(c.to_string(), c.key());
        assert_eq!(c.key().parse::<LocalCategory>().unwrap(), c);
        assert_eq!(c.audit_reason(), format!("toggle:{}", c.key()));
        assert_ne!(c.describe(), "");
    }
}

#[test]
fn signal_codes_are_the_stored_names() {
    let codes: Vec<_> = CompanySignal::ALL.iter().map(|s| s.code()).collect();
    assert_eq!(
        codes,
        [
            "pac_direct",
            "dns_suffix",
            "private_access_cgnat",
            "vpn_route"
        ]
    );
    for s in CompanySignal::ALL {
        assert_eq!(
            serde_json::to_string(&s).unwrap(),
            format!("\"{}\"", s.code())
        );
        assert_eq!(s.to_string(), s.code());
    }
    let described: Vec<_> = CompanySignal::ALL.iter().map(|s| s.describe()).collect();
    assert_eq!(
        described,
        [
            "your company's proxy settings, which send it direct",
            "your company's DNS suffix",
            "a private-access client",
            "your VPN"
        ]
    );
}

#[test]
fn unknown_keys_are_refused() {
    let err = "vpn".parse::<LocalCategory>().unwrap_err();
    assert_eq!(err.what(), "local category");
    assert!("Loopback".parse::<LocalCategory>().is_err());
    assert!(serde_json::from_str::<LocalCategory>("\"lan\"").is_err());
}
