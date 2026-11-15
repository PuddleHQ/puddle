// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn grace_bounds_are_inclusive() {
    assert_eq!(ReconnectionGrace::new(30).unwrap(), ReconnectionGrace::MIN);
    assert_eq!(
        ReconnectionGrace::new(86_400).unwrap(),
        ReconnectionGrace::MAX
    );
    assert!(ReconnectionGrace::new(29).is_err());
    let err = ReconnectionGrace::new(86_401).unwrap_err();
    assert!(err.to_string().contains("between 30 and 86400 s"), "{err}");
}

#[test]
fn grace_display_and_conversions() {
    let g = ReconnectionGrace::DEFAULT;
    assert_eq!(g.to_string(), "300 s");
    assert_eq!(g.duration(), Duration::from_secs(300));
    assert_eq!(u32::from(g), 300);
    assert_eq!(ReconnectionGrace::try_from(60).unwrap().secs(), 60);
}

#[test]
fn grace_validates_when_deserialised() {
    assert_eq!(
        serde_json::from_str::<ReconnectionGrace>("120")
            .unwrap()
            .secs(),
        120
    );
    assert!(serde_json::from_str::<ReconnectionGrace>("0").is_err());
    assert_eq!(
        serde_json::to_string(&ReconnectionGrace::MIN).unwrap(),
        "30"
    );
}

#[test]
fn clipboard_read_is_snake_case_and_defaults_to_ask() {
    assert_eq!(ClipboardRead::default(), ClipboardRead::Ask);
    assert_eq!(
        serde_json::to_string(&ClipboardRead::Allow).unwrap(),
        r#""allow""#
    );
    assert_eq!(
        serde_json::from_str::<ClipboardRead>(r#""deny""#).unwrap(),
        ClipboardRead::Deny
    );
    assert!(serde_json::from_str::<ClipboardRead>(r#""Deny""#).is_err());
}

#[test]
fn choices_are_snake_case_and_default_to_the_safe_option() {
    assert_eq!(ServerChoice::default(), ServerChoice::CodeServer);
    assert_eq!(ThemeChoice::default(), ThemeChoice::System);
    assert_eq!(CloseBehaviour::default(), CloseBehaviour::Tray);
    assert_eq!(DensityChoice::default(), DensityChoice::Comfortable);
    assert_eq!(AppIconChoice::default(), AppIconChoice::Duck);
    assert_eq!(
        serde_json::to_string(&AppIconChoice::DuckPond).unwrap(),
        r#""duck_pond""#
    );
    assert_eq!(
        serde_json::from_str::<AppIconChoice>(r#""custom""#).unwrap(),
        AppIconChoice::Custom
    );
    assert!(serde_json::from_str::<AppIconChoice>(r#""penguin""#).is_err());
    assert_eq!(
        serde_json::from_str::<DensityChoice>(r#""compact""#).unwrap(),
        DensityChoice::Compact
    );
    assert!(serde_json::from_str::<DensityChoice>(r#""tight""#).is_err());
    assert_eq!(PaletteChoice::default(), PaletteChoice::Pond);
    assert_eq!(
        serde_json::from_str::<PaletteChoice>(r#""custom""#).unwrap(),
        PaletteChoice::Custom
    );
    assert!(serde_json::from_str::<PaletteChoice>(r#""neon""#).is_err());
    assert_eq!(
        serde_json::to_string(&ServerChoice::CodeServer).unwrap(),
        r#""code_server""#
    );
    assert_eq!(
        serde_json::from_str::<ServerChoice>(r#""microsoft""#).unwrap(),
        ServerChoice::Microsoft
    );
    assert_eq!(
        serde_json::from_str::<ThemeChoice>(r#""dark""#).unwrap(),
        ThemeChoice::Dark
    );
    assert_eq!(
        serde_json::from_str::<CloseBehaviour>(r#""quit""#).unwrap(),
        CloseBehaviour::Quit
    );
    assert!(serde_json::from_str::<ServerChoice>(r#""other""#).is_err());
}

#[test]
fn colours_are_six_digit_hex_in_lower_case() {
    assert_eq!(HexColour::new("#0A6A85").unwrap().as_str(), "#0a6a85");
    for bad in [
        "", "#fff", "0a6a85", "#0a6a8", "#0a6a850", "#0a6a8g", "#+a6a85",
    ] {
        let err = HexColour::new(bad).unwrap_err();
        assert!(err.to_string().contains("six hexadecimal digits"), "{err}");
    }
    assert_eq!(
        serde_json::to_string(&HexColour::new("#ABCDEF").unwrap()).unwrap(),
        r##""#abcdef""##
    );
    assert!(serde_json::from_str::<HexColour>(r#""teal""#).is_err());
    assert_eq!(
        String::from(serde_json::from_str::<HexColour>(r##""#112233""##).unwrap()),
        "#112233"
    );
}
