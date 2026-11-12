// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;

const DOWN: COREWEBVIEW2_KEY_EVENT_KIND = COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN;

#[test]
fn only_ctrl_q_going_down_alone_is_the_quit_chord() {
    let q = u32::from(b'Q');
    assert!(is_quit_chord(DOWN, q, true, false, false));
    assert!(
        !is_quit_chord(
            COREWEBVIEW2_KEY_EVENT_KIND::default(),
            q,
            true,
            false,
            false
        ),
        "the release"
    );
    assert!(!is_quit_chord(DOWN, q, false, false, false), "Q alone");
    assert!(!is_quit_chord(DOWN, q, true, true, false), "Ctrl+Shift+Q");
    assert!(!is_quit_chord(DOWN, q, true, false, true), "Ctrl+Alt+Q");
    assert!(
        !is_quit_chord(DOWN, u32::from(b'W'), true, false, false),
        "Ctrl+W"
    );
}
