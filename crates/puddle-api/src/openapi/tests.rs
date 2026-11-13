// SPDX-License-Identifier: GPL-3.0-or-later
use std::collections::BTreeSet;

use serde_json::Value;

use super::*;

fn spec() -> Value {
    serde_json::from_str(&openapi_json()).unwrap()
}

#[test]
fn spec_is_openapi_3_1_with_bearer_security() {
    let s = spec();
    assert_eq!(s["openapi"], "3.1.0");
    assert_eq!(s["info"]["version"], API_VERSION);
    assert_eq!(s["security"], serde_json::json!([{ "bearer": [] }]));
    assert_eq!(
        s["components"]["securitySchemes"]["bearer"],
        serde_json::json!({ "type": "http", "scheme": "bearer" })
    );
}

#[test]
fn every_operation_documents_the_guard_refusals() {
    let s = spec();
    let mut count = 0;
    for (path, item) in s["paths"].as_object().unwrap() {
        assert!(path.starts_with("/api/"), "{path}");
        for (method, op) in item.as_object().unwrap() {
            count += 1;
            for status in ["401", "403", "421", "500"] {
                assert!(
                    op["responses"].get(status).is_some(),
                    "{method} {path} lacks {status}"
                );
            }
            assert!(op["tags"].as_array().is_some_and(|t| !t.is_empty()));
        }
    }
    assert_eq!(count, 127, "operations in the spec");
}

/// ADR 0002: responses always carry every field (`null`, never absent), so the generated
/// TypeScript has `T | null`, not `T?`. Request bodies (`*Request`) may leave fields out.
#[test]
fn response_schemas_require_every_property() {
    let s = spec();
    let schemas = s["components"]["schemas"].as_object().unwrap();
    for (name, schema) in schemas {
        if name.ends_with("Request")
            || name == "SettingsLayer"
            || name == "LocalToggles"
            || name == "UiPrefs"
        {
            continue;
        }
        check_required(name, schema);
    }
    // Shared by requests and responses: required in the spec (responses always send them),
    // tolerated when absent in a request.
    for name in ["SettingsLayer", "LocalToggles", "VsCodeServer", "UiPrefs"] {
        check_required(name, &schemas[name]);
    }
}

fn check_required(name: &str, schema: &Value) {
    if let Some(variants) = schema.get("oneOf").and_then(Value::as_array) {
        for v in variants {
            check_required(name, v);
        }
        return;
    }
    let Some(props) = schema.get("properties").and_then(Value::as_object) else {
        return;
    };
    let required: BTreeSet<&str> = schema["required"]
        .as_array()
        .map(|r| r.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    for prop in props.keys() {
        assert!(
            required.contains(prop.as_str()),
            "{name}.{prop} is not required"
        );
    }
}

/// The generated TypeScript names an operation by its id, so two handlers with the same name
/// would silently replace one another's types.
#[test]
fn every_operation_has_its_own_id() {
    let s = spec();
    let mut seen = BTreeSet::new();
    for (path, item) in s["paths"].as_object().unwrap() {
        for (method, op) in item.as_object().unwrap() {
            let id = op["operationId"].as_str().unwrap();
            assert!(
                seen.insert(id.to_owned()),
                "{method} {path} reuses the id {id}"
            );
        }
    }
}

#[test]
fn every_ref_resolves() {
    fn refs(v: &Value, out: &mut Vec<String>) {
        match v {
            Value::Object(map) => {
                if let Some(Value::String(r)) = map.get("$ref") {
                    out.push(r.clone());
                }
                map.values().for_each(|c| refs(c, out));
            }
            Value::Array(items) => items.iter().for_each(|c| refs(c, out)),
            _ => {}
        }
    }
    let s = spec();
    let mut all = Vec::new();
    refs(&s, &mut all);
    assert!(all.len() > 10, "{all:?}");
    for r in all {
        let name = r
            .strip_prefix("#/components/schemas/")
            .unwrap_or_else(|| panic!("{r}"));
        assert!(
            s["components"]["schemas"].get(name).is_some(),
            "{r} does not resolve"
        );
    }
}

#[test]
fn spec_names_no_flatten_or_untagged_shapes() {
    // `allOf` would come from #[serde(flatten)]; `anyOf` from untagged enums (ADR 0002/0004).
    let text = openapi_json();
    assert!(!text.contains("\"allOf\""));
    assert!(!text.contains("\"anyOf\""));
}

#[test]
fn event_union_and_lagged_are_components() {
    let s = spec();
    let schemas = &s["components"]["schemas"];
    assert!(schemas["Event"]["oneOf"].is_array());
    assert!(schemas["Lagged"]["properties"]["missed"].is_object());
    assert!(schemas["ApiErrorBody"].is_object());
    assert!(
        s["paths"]["/api/events"]["get"]["responses"]["200"]["content"]["text/event-stream"]
            .is_object()
    );
}

#[test]
fn committed_spec_is_current() {
    let committed = include_str!("../../openapi/openapi.json");
    assert!(
        committed == openapi_json(),
        "crates/puddle-api/openapi/openapi.json is stale: run `cargo xtask openapi`"
    );
}

/// The only operations whose answer may carry a credential: the access token a sign-in hands the
/// browser that just proved who it is. Their refresh token is in a cookie, not a body.
const GRANTING: &[&str] = &[
    "/api/auth/setup",
    "/api/auth/sign-in",
    "/api/auth/invite",
    "/api/auth/refresh",
];

/// Every schema a response can carry, following references.
fn response_schemas(s: &Value) -> BTreeSet<String> {
    response_schemas_of(s, |path| !GRANTING.contains(&path))
}

/// Every schema the responses of the operations on the paths `keep` accepts can carry.
fn response_schemas_of(s: &Value, keep: impl Fn(&str) -> bool) -> BTreeSet<String> {
    fn refs(value: &Value, out: &mut Vec<String>) {
        match value {
            Value::Object(map) => {
                if let Some(Value::String(r)) = map.get("$ref") {
                    out.push(r.rsplit('/').next().unwrap_or_default().to_owned());
                }
                map.values().for_each(|v| refs(v, out));
            }
            Value::Array(items) => items.iter().for_each(|v| refs(v, out)),
            _ => {}
        }
    }
    let mut todo = Vec::new();
    for (path, item) in s["paths"].as_object().unwrap() {
        if !keep(path) {
            continue;
        }
        for op in item.as_object().unwrap().values() {
            refs(&op["responses"], &mut todo);
        }
    }
    let mut seen = BTreeSet::new();
    while let Some(name) = todo.pop() {
        if seen.insert(name.clone()) {
            refs(&s["components"]["schemas"][&name], &mut todo);
        }
    }
    seen
}

/// No answer of the API can carry a sign-in secret or a token: not a password or its hash, an
/// invite's secret or digest, a session or a device's key. Whoever reads a response (the user
/// list and the admin screen included) never gets one, by construction.
#[test]
fn no_response_can_carry_a_credential() {
    const SECRET_NAMES: &[&str] = &[
        "password",
        "password_hash",
        "hash",
        "digest",
        "invite_secret",
        "invite_digest",
        "session",
        "session_token",
        "access_token",
        "refresh_token",
        "token",
        "device_key",
        "public_key",
        "private_key",
        "secret",
    ];
    fn names(value: &Value, out: &mut BTreeSet<String>) {
        match value {
            Value::Object(map) => {
                if let Some(Value::Object(props)) = map.get("properties") {
                    out.extend(props.keys().cloned());
                }
                map.values().for_each(|v| names(v, out));
            }
            Value::Array(items) => items.iter().for_each(|v| names(v, out)),
            _ => {}
        }
    }
    let s = spec();
    let reachable = response_schemas(&s);
    assert!(reachable.contains("Workspace") && reachable.contains("ApiErrorBody"));
    for name in &reachable {
        let mut props = BTreeSet::new();
        names(&s["components"]["schemas"][name], &mut props);
        for prop in props {
            assert!(
                !SECRET_NAMES.contains(&prop.to_ascii_lowercase().as_str()),
                "response schema {name} has a field {prop}"
            );
        }
    }
}

/// The access token is a field of `SessionGrant` and of nothing else, and `SessionGrant` is the
/// answer of the four sign-in operations alone.
#[test]
fn only_the_sign_in_answers_carry_an_access_token() {
    let s = spec();
    let everywhere = response_schemas_of(&s, |_| true);
    assert!(everywhere.contains("SessionGrant"));
    let elsewhere = response_schemas(&s);
    assert!(
        !elsewhere.contains("SessionGrant"),
        "an answer other than a sign-in carries a session grant"
    );
    for path in GRANTING {
        let only_here = response_schemas_of(&s, |p| p == *path);
        assert!(only_here.contains("SessionGrant"), "{path}");
    }
    for (name, schema) in s["components"]["schemas"].as_object().unwrap() {
        let has = schema["properties"]
            .as_object()
            .is_some_and(|p| p.contains_key("access_token"));
        assert_eq!(has, name == "SessionGrant", "{name}");
    }
}
