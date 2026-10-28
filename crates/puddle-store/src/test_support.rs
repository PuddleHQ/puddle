// SPDX-License-Identifier: GPL-3.0-or-later
//! Helpers the unit tests share.

use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Metadata, Subscriber};

/// Collects the log lines (`LEVEL message field=value ...`) written on this thread while it lives.
pub(crate) struct Logs {
    lines: Arc<Mutex<Vec<String>>>,
    _guard: tracing::subscriber::DefaultGuard,
}

impl Logs {
    pub(crate) fn capture() -> Self {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let subscriber = Capture {
            lines: Arc::clone(&lines),
        };
        Self {
            lines,
            _guard: tracing::subscriber::set_default(subscriber),
        }
    }

    pub(crate) fn lines(&self) -> Vec<String> {
        self.lines.lock().unwrap().clone()
    }
}

struct Capture {
    lines: Arc<Mutex<Vec<String>>>,
}

struct Fields(String);

impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            write!(self.0, " {value:?}").unwrap();
        } else {
            write!(self.0, " {}={value:?}", field.name()).unwrap();
        }
    }
}

impl Subscriber for Capture {
    fn enabled(&self, _: &Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }

    fn record(&self, _: &Id, _: &Record<'_>) {}

    fn record_follows_from(&self, _: &Id, _: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let mut fields = Fields(event.metadata().level().to_string());
        event.record(&mut fields);
        self.lines.lock().unwrap().push(fields.0);
    }

    fn enter(&self, _: &Id) {}

    fn exit(&self, _: &Id) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_events_are_captured_whatever_spans_are_around_them() {
        let logs = Logs::capture();
        let span = tracing::info_span!("outer", answer = tracing::field::Empty);
        let other = tracing::info_span!("other");
        span.record("answer", 42);
        span.follows_from(&other);
        {
            let _inside = span.enter();
            tracing::warn!(count = 3, "inside");
        }
        tracing::info!("outside");
        assert_eq!(logs.lines(), ["WARN inside count=3", "INFO outside"]);
    }
}
