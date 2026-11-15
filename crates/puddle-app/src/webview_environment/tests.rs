// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

/// Reads back what the options object will tell WebView2.
fn read(options: &CoreWebView2EnvironmentOptions) -> (String, String, bool) {
    // SAFETY: plain getters on an object this test owns.
    #[expect(unsafe_code, reason = "webview2-com marks its option getters unsafe")]
    unsafe {
        (
            options.additional_browser_arguments().to_owned(),
            options.language().to_owned(),
            options.is_custom_crash_reporting_enabled(),
        )
    }
}

#[test]
fn the_options_carry_the_browser_arguments_and_turn_custom_crash_reporting_on() {
    let (arguments, language, custom) = read(&options(DEFAULT_ARGUMENTS, "nl-BE"));
    assert_eq!(arguments, DEFAULT_ARGUMENTS);
    assert_eq!(language, "nl-BE");
    assert!(custom, "WebView2 would upload crash dumps to Microsoft");
}

#[test]
fn the_debug_port_arguments_keep_the_two_library_defaults_and_add_the_port() {
    let debug = arguments(Some(9334)).expect("a port gives arguments");
    assert_eq!(
        debug,
        "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection --remote-debugging-port=9334 --remote-allow-origins=*"
    );
    assert_eq!(read(&options(&debug, "")).0, debug);
    assert_eq!(arguments(None), None);
}

#[test]
fn the_default_arguments_are_the_library_ones() {
    assert!(
        DEFAULT_ARGUMENTS
            .starts_with("--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection")
    );
    assert!(DEFAULT_ARGUMENTS.ends_with("--autoplay-policy=no-user-gesture-required"));
}

#[test]
fn the_interface_language_is_a_locale_name_without_padding() {
    let language = ui_language();
    assert!(!language.contains('\0'));
    assert!(language.is_empty() || language.contains('-') || language.len() == 2);
}
