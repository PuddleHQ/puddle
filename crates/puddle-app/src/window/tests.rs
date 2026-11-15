// SPDX-License-Identifier: AGPL-3.0-or-later
use std::sync::Mutex;

use super::*;

fn recorder() -> (OpenExternal, Arc<Mutex<Vec<String>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    let open: OpenExternal = Arc::new(move |u: &Url| sink.lock().unwrap().push(u.to_string()));
    (open, seen)
}

#[test]
fn the_script_sets_a_frozen_token_for_the_api_origin_only() {
    let script = init_script("http://127.0.0.1:4711", "abc123");
    assert!(script.contains("window.location.origin !== \"http://127.0.0.1:4711\""));
    assert!(script.contains("Object.freeze({ token: \"abc123\" })"));
    assert!(script.contains("Object.defineProperty(window, \"__PUDDLE__\""));
    assert_eq!(script.matches("abc123").count(), 1);
}

#[test]
fn the_script_freezes_the_prototype_before_anything_else() {
    let script = init_script("http://127.0.0.1:4711", "abc123");
    assert!(
        script.starts_with("Object.freeze(Object.prototype);"),
        "{script}"
    );
}

#[test]
fn the_script_keeps_hostile_values_inside_their_string_literals() {
    let script = init_script("http://x/\"", "a\"b\\c\nd</script>");
    assert!(script.contains(r#"\"b\\c\nd</script>""#), "{script}");
    assert!(!script.contains("a\"b"), "{script}");
    assert!(script.contains(r#"!== "http://x/\"""#), "{script}");
}

#[test]
fn the_handler_stays_opens_or_drops() {
    let api = Url::parse("http://127.0.0.1:4711/").unwrap();
    let (open, seen) = recorder();
    let (refused, refusals) = recorder();
    let handler = navigation_handler(OriginPolicy::new(&api, None), open, refused);
    assert!(handler(&Url::parse("http://127.0.0.1:4711/inbox").unwrap()));
    assert!(seen.lock().unwrap().is_empty());
    assert!(!handler(&Url::parse("https://example.org/a").unwrap()));
    assert_eq!(*seen.lock().unwrap(), ["https://example.org/a"]);
    assert!(refusals.lock().unwrap().is_empty());
    assert!(!handler(&Url::parse("file:///etc/passwd").unwrap()));
    assert!(!handler(&Url::parse("http://tauri.localhost/").unwrap()));
    assert_eq!(seen.lock().unwrap().len(), 1);
    assert_eq!(
        *refusals.lock().unwrap(),
        ["file:///etc/passwd", "http://tauri.localhost/"]
    );
}

#[test]
fn a_new_window_opens_the_web_reports_the_refused_and_drops_the_rest() {
    let api = Url::parse("http://127.0.0.1:4711/").unwrap();
    let policy = OriginPolicy::new(&api, None);
    let (open, opened) = recorder();
    let (refused, refusals) = recorder();
    for target in [
        "https://example.org/a",
        "mailto:a@example.org",
        "http://127.0.0.1:4711/inbox",
    ] {
        new_window_handler(&policy, &Url::parse(target).unwrap(), &open, &refused);
    }
    assert_eq!(*opened.lock().unwrap(), ["https://example.org/a"]);
    assert_eq!(*refusals.lock().unwrap(), ["mailto:a@example.org"]);
}

#[test]
fn the_main_window_opens_once_on_the_api_origin() {
    let app = tauri::test::mock_app();
    let url = Url::parse("http://127.0.0.1:4711/").unwrap();
    let (open, _) = recorder();
    let (refused, _) = recorder();
    let first = open_main(&app, &url, "t", None, None, open.clone(), refused.clone()).unwrap();
    assert_eq!(first.label(), MAIN_LABEL);
    assert_eq!(first.url().unwrap(), url);
    let second = open_main(&app, &url, "t", None, None, open, refused).unwrap();
    assert_eq!(second.label(), MAIN_LABEL);
    assert_eq!(app.webview_windows().len(), 1);
}

#[test]
fn a_development_server_replaces_the_start_page() {
    let app = tauri::test::mock_app();
    let api = Url::parse("http://127.0.0.1:4711/").unwrap();
    let dev = Url::parse("http://localhost:5173/").unwrap();
    let (open, _) = recorder();
    let (refused, _) = recorder();
    let window = open_main(&app, &api, "t", Some(&dev), None, open, refused).unwrap();
    assert_eq!(window.url().unwrap(), dev);
}
