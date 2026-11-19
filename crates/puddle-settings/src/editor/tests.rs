// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use crate::Source;

fn theme(n: &str) -> ColourTheme {
    ColourTheme::new(n).unwrap()
}

fn ext(n: &str) -> ExtensionId {
    ExtensionId::new(n).unwrap()
}

#[test]
fn nothing_set_resolves_to_the_defaults() {
    let e = EffectiveEditor::resolve(&EditorLayer::default(), &EditorLayer::default());
    assert_eq!(e, EffectiveEditor::DEFAULTS);
    assert_eq!(e.colour_theme.source, Source::Default);
    assert!(e.extensions.value.is_empty());
}

#[test]
fn a_workspace_wins_field_by_field() {
    let global = EditorLayer {
        colour_theme: Some(theme("Solarized Dark")),
        font_size: Some(EditorFontSize::new(16).unwrap()),
        extensions: Some(vec![ext("a.one")]),
        ..EditorLayer::default()
    };
    let workspace = EditorLayer {
        font_size: Some(EditorFontSize::new(12).unwrap()),
        extensions: Some(vec![ext("b.two")]),
        ..EditorLayer::default()
    };
    let e = EffectiveEditor::resolve(&global, &workspace);
    assert_eq!(e.colour_theme.value, Some(theme("Solarized Dark")));
    assert_eq!(e.colour_theme.source, Source::Global);
    assert_eq!(e.font_size.value.map(EditorFontSize::get), Some(12));
    assert_eq!(e.font_size.source, Source::Workspace);
    assert_eq!(e.extensions.value, vec![ext("b.two")]);
    assert_eq!(e.extensions.source, Source::Workspace);
    assert_eq!(e.keybindings.source, Source::Default);
}

#[test]
fn an_empty_workspace_extension_list_clears_the_global_one() {
    let global = EditorLayer {
        extensions: Some(vec![ext("a.one")]),
        ..EditorLayer::default()
    };
    let workspace = EditorLayer {
        extensions: Some(Vec::new()),
        ..EditorLayer::default()
    };
    let e = EffectiveEditor::resolve(&global, &workspace);
    assert!(e.extensions.value.is_empty());
    assert_eq!(e.extensions.source, Source::Workspace);
}

#[test]
fn values_are_checked_when_read() {
    for bad in [
        r#"{"colour_theme":""}"#,
        r#"{"colour_theme":"a\nb"}"#,
        r#"{"font_size":7}"#,
        r#"{"font_size":41}"#,
        r#"{"extensions":["nope"]}"#,
        r#"{"extensions":["a.b --force"]}"#,
        r#"{"keybindings":"{}"}"#,
        r#"{"keybindings":"[{\"key\":\"a\"}]"}"#,
    ] {
        assert!(serde_json::from_str::<EditorLayer>(bad).is_err(), "{bad}");
    }
}

#[test]
fn a_layer_round_trips_and_unknown_fields_are_kept() {
    let text = r#"{"colour_theme":"Default Dark Modern","font_size":14,"extensions":["rust-lang.rust-analyzer"],"keybindings":"[{\"key\":\"ctrl+j\",\"command\":\"-workbench.action.togglePanel\"}]","later":1}"#;
    let layer: EditorLayer = serde_json::from_str(text).unwrap();
    assert!(!layer.is_empty());
    let mut unknown = Vec::new();
    layer.collect_unknown("editor.", &mut unknown);
    assert_eq!(unknown, vec!["editor.later".to_owned()]);
    let back: EditorLayer = serde_json::from_str(&serde_json::to_string(&layer).unwrap()).unwrap();
    assert_eq!(back, layer);
}

#[test]
fn keybindings_over_the_cap_are_refused() {
    let one = r#"{"key":"a","command":"b"},"#;
    let text = format!("[{}{{\"key\":\"a\",\"command\":\"b\"}}]", one.repeat(3000));
    assert!(text.len() > Keybindings::MAX_BYTES);
    assert!(Keybindings::new(&text).is_err());
}
