// SPDX-License-Identifier: AGPL-3.0-or-later
use serde_json::json;

use super::*;

fn complete(steps: &[Migration], current: u32) {
    let starts: Vec<u32> = steps.iter().map(|m| m.from).collect();
    let want: Vec<u32> = (1..current).collect();
    assert_eq!(starts, want, "one migration per version, in order");
}

#[test]
fn every_kind_has_a_complete_chain() {
    complete(GLOBAL, crate::GLOBAL_SCHEMA_VERSION);
    complete(WORKSPACE, crate::WORKSPACE_SCHEMA_VERSION);
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "has the signature of Migration::apply"
)]
fn rename_a_to_b(m: &mut Map<String, Value>) -> Result<(), String> {
    if let Some(v) = m.remove("a") {
        m.insert("b".to_owned(), v);
    }
    Ok(())
}

fn double_b(m: &mut Map<String, Value>) -> Result<(), String> {
    let Some(b) = m.get("b") else { return Ok(()) };
    let n = b.as_u64().ok_or("b is not a number")?;
    m.insert("b".to_owned(), json!(n * 2));
    Ok(())
}

const STEPS: &[Migration] = &[
    Migration {
        from: 1,
        apply: rename_a_to_b,
    },
    Migration {
        from: 2,
        apply: double_b,
    },
];

#[test]
fn steps_run_in_order_from_the_stored_version() {
    let (m, found) = upgrade("t", json!({"schema_version":1,"a":5}), 3, STEPS).unwrap();
    assert_eq!(found, 1);
    assert_eq!(Value::Object(m), json!({"b":10}));

    let (m, found) = upgrade("t", json!({"schema_version":2,"b":5}), 3, STEPS).unwrap();
    assert_eq!(found, 2);
    assert_eq!(Value::Object(m), json!({"b":10}));

    let (m, found) = upgrade("t", json!({"schema_version":3,"b":5}), 3, STEPS).unwrap();
    assert_eq!(found, 3);
    assert_eq!(Value::Object(m), json!({"b":5}));
}

#[test]
fn a_missing_version_is_version_one() {
    let (m, found) = upgrade("t", json!({"a":1}), 3, STEPS).unwrap();
    assert_eq!(found, 1);
    assert_eq!(Value::Object(m), json!({"b":2}));
}

#[test]
fn a_failing_step_names_its_version() {
    let err = upgrade("t", json!({"schema_version":2,"b":"x"}), 3, STEPS).unwrap_err();
    assert_eq!(
        err.to_string(),
        "cannot migrate t settings from schema version 2: b is not a number"
    );
}

#[test]
fn a_gap_in_the_chain_is_an_error_not_a_skip() {
    let err = upgrade("t", json!({"schema_version":1}), 3, &STEPS[1..]).unwrap_err();
    assert!(
        matches!(err, SettingsError::Migration { from: 1, .. }),
        "{err}"
    );
}

#[test]
fn newer_documents_are_refused() {
    let err = upgrade("t", json!({"schema_version":4}), 3, STEPS).unwrap_err();
    assert_eq!(
        err,
        SettingsError::NewerSchema {
            kind: "t",
            found: 4,
            supported: 3
        }
    );
    assert!(err.to_string().contains("newer Puddle"), "{err}");
}

#[test]
fn bad_versions_and_non_objects_are_refused() {
    for bad in [
        json!(0),
        json!(-1),
        json!(1.5),
        json!("1"),
        json!(null),
        json!(u64::from(u32::MAX) + 1),
    ] {
        let err = upgrade("t", json!({ "schema_version": bad }), 3, STEPS).unwrap_err();
        assert!(
            matches!(err, SettingsError::InvalidVersion { .. }),
            "{bad}: {err}"
        );
    }
    let err = upgrade("t", json!({ "schema_version": "x".repeat(100) }), 3, STEPS).unwrap_err();
    assert!(err.to_string().len() < 120, "{err}");
    for bad in [json!([]), json!(1), json!(null), json!("{}")] {
        assert_eq!(
            upgrade("t", bad, 3, STEPS).unwrap_err(),
            SettingsError::NotAnObject { kind: "t" }
        );
    }
}
