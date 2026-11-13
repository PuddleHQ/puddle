// SPDX-License-Identifier: AGPL-3.0-or-later
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
fn uniform_applies_one_set_of_targets_to_every_crate() {
    let f = ScopedFilter::uniform("debug".parse().unwrap());
    assert!(on(&f, "hyper", Level::DEBUG));
    assert!(on(&f, "puddle_app", Level::DEBUG));
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
