// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn v(s: &str) -> TermsVersion {
    TermsVersion::new(s).unwrap()
}

#[test]
fn terms_version_rules() {
    assert!(TermsVersion::new(&"x".repeat(256)).is_ok());
    let long = TermsVersion::new(&"x".repeat(257)).unwrap_err();
    assert_eq!(long.reason(), "is too long");
    assert_eq!(
        TermsVersion::new("a\u{e9}").unwrap_err().reason(),
        "may only contain visible ASCII characters"
    );
    assert_eq!(
        TermsVersion::new("").unwrap_err().reason(),
        "must not be empty"
    );
    assert_eq!(v("https://x/terms").to_string(), "https://x/terms");
    assert_eq!(String::from(v("v1")), "v1");
    assert!(serde_json::from_str::<TermsVersion>(r#""a b""#).is_err());
}

#[test]
fn consent_wire_shape_is_tagged_by_state() {
    let g = Consent::Granted {
        at: UnixMillis(1_700_000_000_000),
        terms_version: v("v1"),
    };
    assert_eq!(
        serde_json::to_string(&g).unwrap(),
        r#"{"state":"granted","at":1700000000000,"terms_version":"v1"}"#
    );
    assert_eq!(
        serde_json::to_string(&Consent::NotAsked).unwrap(),
        r#"{"state":"not_asked"}"#
    );
    let d: Consent =
        serde_json::from_str(r#"{"state":"declined","at":5,"terms_version":"v1"}"#).unwrap();
    assert_eq!(
        d,
        Consent::Declined {
            at: UnixMillis(5),
            terms_version: v("v1")
        }
    );
    assert!(serde_json::from_str::<Consent>(r#"{"state":"granted","at":5}"#).is_err());
    assert!(serde_json::from_str::<Consent>(r#"{"state":"revoked"}"#).is_err());
}

#[test]
fn only_a_grant_of_the_current_terms_allows() {
    let cur = v("v2");
    assert!(!Consent::NotAsked.allows(&cur));
    let declined = Consent::Declined {
        at: UnixMillis(1),
        terms_version: cur.clone(),
    };
    assert!(!declined.allows(&cur));
    assert!(!declined.should_ask(&cur));
    assert!(!declined.should_ask(&v("v3")));
    let granted = Consent::Granted {
        at: UnixMillis(1),
        terms_version: cur.clone(),
    };
    assert!(granted.allows(&cur));
    assert!(!granted.should_ask(&cur));
}

#[test]
fn consents_get_and_set_each_kind() {
    let mut c = Consents::default();
    for kind in ConsentKind::ALL {
        assert_eq!(c.get(kind), &Consent::NotAsked);
        let rec = Consent::Granted {
            at: UnixMillis(7),
            terms_version: v("v1"),
        };
        assert_eq!(c.set(kind, rec.clone()), Consent::NotAsked);
        assert_eq!(c.get(kind), &rec);
    }
    assert_eq!(
        serde_json::to_string(&ConsentKind::VsCodeServer).unwrap(),
        r#""vscode_server""#
    );
}

#[test]
fn not_asked_consents_are_left_out_of_the_document() {
    let mut c = Consents::default();
    assert_eq!(serde_json::to_string(&c).unwrap(), "{}");
    c.set(
        ConsentKind::Telemetry,
        Consent::Declined {
            at: UnixMillis(3),
            terms_version: v("t1"),
        },
    );
    assert_eq!(
        serde_json::to_string(&c).unwrap(),
        r#"{"telemetry":{"state":"declined","at":3,"terms_version":"t1"}}"#
    );
}
