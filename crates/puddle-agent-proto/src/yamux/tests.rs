// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn both_sides_use_the_16_mib_window() {
    assert_eq!(client_config().max_stream_window_size, STREAM_WINDOW);
    assert_eq!(server_config().max_stream_window_size, STREAM_WINDOW);
    assert_eq!(server_config().accept_backlog, ACCEPT_BACKLOG);
    assert!(server_config().enable_keepalive);
}

#[test]
fn a_window_below_the_yamux_minimum_is_raised() {
    assert_eq!(
        client_config_with_window(1).max_stream_window_size,
        256 * 1024
    );
    assert_eq!(
        client_config_with_window(32 << 20).max_stream_window_size,
        32 << 20
    );
}
