// SPDX-License-Identifier: AGPL-3.0-or-later
use std::io::Write;

use super::*;

#[test]
fn captures_written_bytes_and_flushes() {
    let mut logs = CapturedLogs::default();
    logs.write_all(b"one line\n").unwrap();
    logs.flush().unwrap();
    assert_eq!(logs.text(), "one line\n");
}

#[test]
fn captures_a_logged_event() {
    let (logs, _guard) = capture_logs();
    tracing::debug!("peer dropped");
    assert!(logs.text().contains("peer dropped"));
}
