// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn policy() -> OriginPolicy {
    OriginPolicy::new(&Url::parse("http://127.0.0.1:4711/").unwrap(), None)
}

fn class(policy: &OriginPolicy, url: &str) -> Navigation {
    policy.classify(&Url::parse(url).unwrap())
}

#[test]
fn the_api_origin_stays() {
    let p = policy();
    for url in [
        "http://127.0.0.1:4711/",
        "http://127.0.0.1:4711/inbox?x=1#y",
        "http://127.0.0.1:4711/_app/immutable/a.js",
        "about:blank",
        "blob:http://127.0.0.1:4711/2f1c",
    ] {
        assert_eq!(class(&p, url), Navigation::Stay, "{url}");
    }
}

#[test]
fn another_origin_of_the_same_machine_is_external() {
    let p = policy();
    for url in [
        "http://127.0.0.1:4712/",
        "http://127.0.0.1/",
        "https://127.0.0.1:4711/",
        "http://localhost:4711/",
        "http://[::1]:4711/",
        "https://example.org/docs",
        "http://workspace.localhost:9000/",
        "http://127.0.0.1.example.org:4711/",
        "http://127.0.0.1:4711@example.org/",
    ] {
        assert_eq!(class(&p, url), Navigation::External, "{url}");
    }
}

#[test]
fn the_webview_s_own_names_and_non_web_schemes_are_refused() {
    let p = policy();
    for url in [
        "http://tauri.localhost/index.html",
        "http://TAURI.localhost:4711/",
        "http://ipc.localhost/",
        "https://asset.localhost/x",
        "tauri://localhost/",
        "ipc://localhost/",
        "asset://localhost/x",
        "file:///c:/windows/win.ini",
        "javascript:alert(1)",
        "data:text/html,hi",
        "mailto:a@example.org",
        "ms-msdt:something",
        "about:srcdoc",
    ] {
        assert_eq!(class(&p, url), Navigation::Refuse, "{url}");
    }
}

#[test]
fn a_development_server_stays_only_when_given() {
    let dev = Url::parse("http://localhost:5173/").unwrap();
    let with = OriginPolicy::new(&Url::parse("http://127.0.0.1:4711/").unwrap(), Some(&dev));
    assert_eq!(
        class(&with, "http://localhost:5173/inbox"),
        Navigation::Stay
    );
    assert_eq!(class(&with, "http://localhost:5174/"), Navigation::External);
    assert_eq!(
        class(&policy(), "http://localhost:5173/inbox"),
        Navigation::External
    );
}
