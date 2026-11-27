// SPDX-License-Identifier: AGPL-3.0-or-later
use std::io::Write as _;

use super::*;

fn on(filter: &ScopedFilter, target: &str, level: Level) -> bool {
    filter.would_enable(target, &level)
}

#[test]
fn trace_reaches_puddle_crates_and_stops_at_info_for_the_rest() {
    let f = ScopedFilter::parse("trace").unwrap();
    for target in [
        "puddle",
        "puddle::cmd::serve",
        "puddle_proxy",
        "puddle_host::logins",
    ] {
        assert!(on(&f, target, Level::TRACE), "{target}");
    }
    for target in [
        "hyper::proto::h1",
        "rustls::client",
        "h2::codec",
        "tokio",
        "puddled",
    ] {
        assert!(on(&f, target, Level::INFO), "{target}");
        assert!(!on(&f, target, Level::DEBUG), "{target}");
    }
}

#[test]
fn a_dependency_named_in_the_text_gets_the_level_named() {
    let f = ScopedFilter::parse("debug, hyper=trace").unwrap();
    assert!(on(&f, "hyper::proto", Level::TRACE));
    assert!(!on(&f, "h2::codec", Level::DEBUG));
    assert!(on(&f, "puddle_ssh", Level::DEBUG));
    assert!(!on(&f, "puddle_ssh", Level::TRACE));
    let lower = ScopedFilter::parse("puddle_ssh=warn,debug").unwrap();
    assert!(!on(&lower, "puddle_ssh", Level::INFO));
    assert!(on(&lower, "puddle_host", Level::DEBUG));
}

#[test]
fn a_level_below_info_holds_the_dependencies_down_too() {
    let f = ScopedFilter::parse("warn").unwrap();
    assert!(!on(&f, "puddle_host", Level::INFO));
    assert!(!on(&f, "hyper", Level::INFO));
    assert!(on(&f, "hyper", Level::WARN));
}

#[test]
fn no_text_and_empty_text_mean_info() {
    for text in ["", " , "] {
        let f = ScopedFilter::parse(text).unwrap();
        assert!(on(&f, "puddle_host", Level::INFO));
        assert!(!on(&f, "puddle_host", Level::DEBUG));
    }
}

#[test]
fn text_that_is_not_a_filter_is_refused() {
    for text in ["loud", "debug,hyper=loud", "=debug", "trace;hyper"] {
        assert_eq!(ScopedFilter::parse(text).unwrap_err(), BadFilter, "{text}");
    }
}

#[test]
fn the_level_hint_is_the_most_verbose_of_the_two_scopes() {
    let f = ScopedFilter::parse("trace").unwrap();
    assert_eq!(
        Filter::<tracing_subscriber::Registry>::max_level_hint(&f),
        Some(LevelFilter::TRACE)
    );
    let f = ScopedFilter::parse("warn").unwrap();
    assert_eq!(
        Filter::<tracing_subscriber::Registry>::max_level_hint(&f),
        Some(LevelFilter::WARN)
    );
}

#[test]
fn a_line_goes_through_the_filter_by_its_crate() {
    use std::sync::{Arc, Mutex};
    #[derive(Clone, Default)]
    struct Buf(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let buf = Buf::default();
    let writer = buf.clone();
    let subscriber = tracing_subscriber::registry().with(
        tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .with_filter(ScopedFilter::parse("trace").unwrap()),
    );
    tracing::subscriber::with_default(subscriber, || {
        tracing::trace!(target: "puddle_proxy::x", "from the proxy");
        tracing::trace!(target: "hyper::x", "from hyper trace");
        tracing::info!(target: "hyper::x", "from hyper info");
    });
    std::io::Write::flush(&mut buf.clone()).unwrap();
    let log = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    assert!(
        log.contains("from the proxy") && log.contains("from hyper info"),
        "{log}"
    );
    assert!(!log.contains("from hyper trace"), "{log}");
}

// Assembled at compile time so the secret scan finds no token-shaped literal in the tree.
const GITHUB_TOKEN: &str = concat!("gh", "p_aB3dE6gH9jK2mN5pQ8sT1vW4yZ7bC0eF3hJ6");
const JWT: &str = concat!(
    "ey",
    "JhbGciOiJIUzI1NiJ9.",
    "ey",
    "JzdWIiOiIxMjM0NTY3ODkwIn0.dBjftJeZ4CVPmB92K27uhbUJU1p1r"
);

const SECRETS: [&str; 9] = [
    GITHUB_TOKEN,
    "sk-ant-api03-Zx9Qw8Er7Ty6Ui5Op4As3Df2Gh1Jk0Lm",
    "hunter2hunter2",
    "s3cr3tCookieValue",
    JWT,
    "pa55w0rd",
    "AKIAIOSFODNN7EXAMPLE",
    "queryTokenValue",
    "bearerValue123",
];

fn leaky_lines() -> String {
    [
        &format!("proxy request token={GITHUB_TOKEN} host=github.com"),
        "using key sk-ant-api03-Zx9Qw8Er7Ty6Ui5Op4As3Df2Gh1Jk0Lm for upstream",
        r#"body {"password":"hunter2hunter2","user":"ada"}"#,
        "sent Authorization: Bearer bearerValue123 and Cookie: session=s3cr3tCookieValue",
        &format!("jwt {JWT} end"),
        "clone https://ada:pa55w0rd@github.com/org/repo.git",
        "aws AKIAIOSFODNN7EXAMPLE",
        "GET https://api.example.com/v1/items?access=queryTokenValue 200",
        "password: pa55w0rd",
    ]
    .join("\n")
}

#[test]
fn redact_cuts_every_secret_shape_and_keeps_the_rest() {
    let out = redact(&leaky_lines());
    for secret in SECRETS {
        assert!(!out.contains(secret), "a secret survived redaction");
    }
    for kept in [
        "host=github.com",
        "github.com/org/repo.git",
        "api.example.com/v1/items",
        "200",
    ] {
        assert!(out.contains(kept), "{kept} was cut from:\n{out}");
    }
}

#[test]
fn redact_json_cuts_values_under_secret_keys_and_inside_strings() {
    let mut doc = serde_json::json!({
        "ui": { "theme": "dark" },
        "github": { "token": GITHUB_TOKEN, "unset_token": null },
        "note": "see https://ada:pa55w0rd@host/x",
    });
    redact_json(&mut doc);
    let text = doc.to_string();
    assert!(
        !text.contains("ghp_") && !text.contains("pa55w0rd"),
        "{text}"
    );
    assert_eq!(doc["ui"]["theme"], "dark");
    assert_eq!(doc["github"]["unset_token"], serde_json::Value::Null);
}

#[test]
fn a_bundle_holds_the_logs_and_settings_with_no_secret_in_any_file() {
    let data = tempfile::tempdir().unwrap();
    let folder = log_folder(data.path());
    let file = RollingFile::default();
    file.attach(&folder, MAX_FILE_BYTES).unwrap();
    std::fs::write(
        folder.join(format!("{FILE_NAME}.1")),
        format!("old line /home/ada/work token={GITHUB_TOKEN}\n"),
    )
    .unwrap();
    std::fs::OpenOptions::new()
        .append(true)
        .open(folder.join(FILE_NAME))
        .unwrap()
        .write_all(leaky_lines().as_bytes())
        .unwrap();
    let mut out = Vec::new();
    write_bundle(
        &Bundle {
            versions: &[("puddle".into(), "0.1.0".into())],
            platform: "linux x86_64",
            log_level: "debug",
            settings: Some(serde_json::json!({
                "ui": { "theme": "dark" },
                "secrets": { "pasted_token": GITHUB_TOKEN },
            })),
            log_folder: &folder,
            home: Some("/home/ada"),
            generated: 1_700_000_000,
        },
        &mut out,
    )
    .unwrap();

    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(out.as_slice()));
    let mut files = std::collections::BTreeMap::new();
    for entry in archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        let name = entry.path().unwrap().to_string_lossy().into_owned();
        let mut text = String::new();
        std::io::Read::read_to_string(&mut entry, &mut text).unwrap();
        files.insert(name, text);
    }
    let names: Vec<_> = files.keys().map(String::as_str).collect();
    assert_eq!(
        names,
        [
            "info.txt",
            "logs/puddle.log",
            "logs/puddle.log.1",
            "settings.json"
        ]
    );
    for (name, text) in &files {
        for secret in SECRETS {
            assert!(!text.contains(secret), "a secret is in {name}");
        }
        assert!(!text.contains("/home/ada"), "home folder in {name}");
    }
    assert!(files["info.txt"].contains("puddle 0.1.0"));
    assert!(files["logs/puddle.log.1"].contains("old line ~/work"));
    assert!(files["settings.json"].contains("dark"));
}

#[test]
fn the_log_file_rolls_over_at_its_size_and_keeps_one_old_file() {
    let data = tempfile::tempdir().unwrap();
    let file = RollingFile::default();
    file.attach(data.path(), 10).unwrap();
    let mut w = file::RollingWriter::of(&file);
    w.write_all(b"0123456789").unwrap();
    w.write_all(b"next").unwrap();
    assert_eq!(
        std::fs::read_to_string(data.path().join("puddle.log.1")).unwrap(),
        "0123456789"
    );
    assert_eq!(
        std::fs::read_to_string(data.path().join("puddle.log")).unwrap(),
        "next"
    );
}

#[test]
fn the_setting_changes_the_level_of_a_running_log_unless_the_environment_pinned_it() {
    // The one test that starts logging for this process: the control is a process-wide slot.
    assert!(init_logging(ScopedFilter::default(), false, false));
    let on = |l: Level| {
        let control = CONTROL.get().unwrap();
        let filter = control.filter.0.read().unwrap();
        filter.would_enable("puddle_host", &l)
    };
    assert!(!on(Level::DEBUG));
    assert!(apply_setting(LevelFilter::DEBUG));
    assert!(on(Level::DEBUG) && !on(Level::TRACE));
    assert!(
        !apply_setting(LevelFilter::DEBUG),
        "same level is no change"
    );
    assert!(!level_is_pinned());
}
