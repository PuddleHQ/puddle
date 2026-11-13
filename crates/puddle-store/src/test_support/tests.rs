// SPDX-License-Identifier: AGPL-3.0-or-later
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
