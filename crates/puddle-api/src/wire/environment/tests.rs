// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn a_secret_value_never_shows_in_debug_output_or_in_the_refusal_of_a_wrong_type() {
    let request: EnvSetRequest =
        serde_json::from_str(r#"{"kind":"secret","value":"CANARY-9","hosts":["api.example.com"]}"#)
            .unwrap();
    let shown = format!("{request:?}");
    assert!(!shown.contains("CANARY-9"), "{shown}");
    assert!(shown.contains("<redacted>"), "{shown}");

    for wrong in [
        "12345678",
        "1.5",
        "true",
        r#"["CANARY-9"]"#,
        r#"{"v":"CANARY-9"}"#,
        "-12345678",
    ] {
        let body = format!(r#"{{"kind":"secret","value":{wrong},"hosts":["a.example.com"]}}"#);
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
        let err = EnvSetRequest::deserialize(value).unwrap_err().to_string();
        assert!(err.contains("must be a string"), "{err}");
        assert!(
            !err.contains("12345678") && !err.contains("CANARY"),
            "{err}"
        );
    }
    // A kind of value serde_json never produces still gets a refusal that quotes nothing.
    let bytes = de::value::BytesDeserializer::<de::value::Error>::new(b"CANARY-bytes");
    let err = SecretText::deserialize(bytes).err().unwrap().to_string();
    assert!(err.contains("a string") && !err.contains("CANARY"), "{err}");
    // The value may be left out: the secret keeps what it has.
    assert!(matches!(
        serde_json::from_str::<EnvSetRequest>(r#"{"kind":"secret","hosts":["a.example.com"]}"#)
            .unwrap(),
        EnvSetRequest::Secret { value: None, .. }
    ));
    // Unknown fields and kinds are refused.
    assert!(
        serde_json::from_str::<EnvSetRequest>(r#"{"kind":"plain","value":"x","extra":1}"#).is_err()
    );
    assert!(serde_json::from_str::<EnvSetRequest>(r#"{"kind":"other"}"#).is_err());
}

#[test]
fn a_secret_is_listed_by_name_and_hosts_and_never_by_value() {
    let db = store::Store::open_in_memory(
        std::sync::Arc::new(store::ManualClock::new(1)),
        store::Limits::default(),
    )
    .unwrap();
    let draft = store::EnvDraft::secret(
        puddle_secrets::StoredId::new("env-1").unwrap(),
        store::SecretHost::list(&["api.example.com"]).unwrap(),
    )
    .unwrap();
    let secret = db
        .set_env(
            puddle_store::UserId::OWNER,
            &store::EnvScope::Global,
            &store::EnvName::new("T").unwrap(),
            draft,
        )
        .unwrap()
        .entry;
    let view = EnvVariable::from_store(secret, false);
    assert_eq!(view.kind, EnvKind::Secret);
    assert_eq!(view.value, None);
    assert_eq!(view.hosts, ["api.example.com"]);
    assert_eq!(view.scope, EnvScope::Global);
    let json = serde_json::to_string(&view).unwrap();
    assert!(json.contains(r#""value":null"#), "{json}");
    assert!(
        !json.contains("env-1"),
        "the credential store's id is not shown: {json}"
    );
}
