// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn colours_have_one_spelling() {
    assert_eq!("#ABCDEF".parse::<Rgb>().unwrap().to_string(), "#abcdef");
    for bad in [
        "",
        "#abc",
        "abcdef",
        "#abcdeg",
        "#abcdef0",
        "#abcdef00",
        "red",
        "# 12345",
    ] {
        assert!(bad.parse::<Rgb>().is_err(), "{bad:?}");
    }
    // A multi-byte character must not panic the slicing.
    assert!("#éééé".parse::<Rgb>().is_err());
}

#[test]
fn a_fill_round_trips_and_refuses_unknown_kinds() {
    let fill = Fill::solid(Rgb::new(1, 2, 3));
    let json = serde_json::to_value(fill).unwrap();
    assert_eq!(
        json,
        serde_json::json!({"kind": "solid", "color": "#010203"})
    );
    assert_eq!(serde_json::from_value::<Fill>(json).unwrap(), fill);
    assert_eq!(fill.to_json(), serde_json::to_string(&fill).unwrap());
    assert!(serde_json::from_str::<Fill>(r##"{"kind":"conic","color":"#010203"}"##).is_err());
    assert!(serde_json::from_str::<Fill>(r#"{"kind":"solid","color":"nope"}"#).is_err());
    assert!(serde_json::from_str::<Fill>(r##""#010203""##).is_err());
}

#[test]
fn user_sets_without_a_choice_cycle_through_the_palette() {
    let first = Fill::default_for_user_set(0).primary();
    assert_eq!(first, SET_PALETTE[0]);
    assert_eq!(Fill::default_for_user_set(8).primary(), first);
    assert_eq!(Fill::default_for_user_set(3).primary(), SET_PALETTE[3]);
    assert_ne!(Fill::default_for_user_set(1), Fill::default_for_user_set(2));
    // A negative id never occurs, but must not panic.
    let _ = Fill::default_for_user_set(i64::MIN);
}

#[cfg(feature = "openapi")]
#[test]
fn the_rgb_schema_is_a_hex_colour_string() {
    use utoipa::PartialSchema;

    let schema = serde_json::to_value(Rgb::schema()).unwrap();
    assert_eq!(schema["type"], "string");
    assert_eq!(schema["pattern"], "^#[0-9a-fA-F]{6}$");
    assert_eq!(schema["examples"], serde_json::json!(["#0a6a85"]));
    assert_eq!(schema["description"], "A colour as `#rrggbb`.");
}
