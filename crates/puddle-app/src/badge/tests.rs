// SPDX-License-Identifier: AGPL-3.0-or-later
use puddle_types::{PendingEnd, PendingSummary, WorkspaceId};

use super::*;

fn opened(id: i64) -> Event {
    Event::PendingOpened {
        request: PendingSummary {
            id,
            workspace: WorkspaceId::new("demo").unwrap(),
            host: "example.org".to_owned(),
            registrable_domain: "example.org".to_owned(),
            port: 443,
            first_seen: 1,
            last_seen: 1,
            attempts: 1,
        },
    }
}

fn closed(id: i64) -> Event {
    Event::PendingClosed {
        workspace: WorkspaceId::new("demo").unwrap(),
        id,
        state: PendingEnd::Allowed,
        rule_id: Some(1),
    }
}

#[test]
fn the_count_follows_opened_and_closed_requests_once_each() {
    let mut open = OpenRequests::of([7]);
    assert_eq!(open.count(), 1);
    assert!(open.apply(&opened(8)));
    assert!(
        !open.apply(&opened(8)),
        "the same request twice counts once"
    );
    assert_eq!(open.count(), 2);
    assert!(open.apply(&closed(7)));
    assert!(!open.apply(&closed(7)), "closing it again changes nothing");
    assert!(!open.apply(&Event::RulesChanged {}));
    assert_eq!(open.count(), 1);
}

#[test]
fn the_title_and_tooltip_show_the_count_only_with_the_badge_on_and_something_waiting() {
    assert_eq!(window_title(3, true), "Puddle (3)");
    assert_eq!(window_title(0, true), "Puddle");
    assert_eq!(window_title(3, false), "Puddle");
    assert_eq!(window_title(250, true), "Puddle (99+)");
    assert_eq!(tooltip(1, true), "Puddle: 1 request is waiting");
    assert_eq!(tooltip(2, true), "Puddle: 2 requests are waiting");
    assert_eq!(tooltip(2, false), "Puddle");
    assert_eq!(tooltip(0, true), "Puddle");
}

#[test]
fn the_menu_line_always_says_what_waits() {
    assert_eq!(waiting_line(0), "Nothing is waiting");
    assert_eq!(waiting_line(1), "1 request is waiting");
    assert_eq!(waiting_line(12), "12 requests are waiting");
}

#[test]
fn the_overlay_is_drawn_at_the_size_windows_shows_for_the_display_scale() {
    for (scale, side) in [(1.0, 16), (1.25, 20), (1.5, 24), (2.0, 32)] {
        let overlay = overlay_number(3, scale).unwrap();
        assert_eq!(
            (overlay.size, overlay.rgba.len()),
            (side, (side * side * 4) as usize)
        );
    }
}

#[test]
fn the_tray_dot_has_no_white_ring_unless_the_switch_is_on() {
    let base = Icon {
        size: 32,
        rgba: [0x20, 0x40, 0x60, 0xff].repeat(32 * 32),
    };
    let marked = with_dot(&base).unwrap();
    let white = marked
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[..3] == [0xff; 3])
        .count();
    assert_eq!(white > 0, TRAY_RING);
    assert_ne!(marked, base);
    assert!(
        with_dot(&Icon {
            size: 4,
            rgba: vec![0; 10]
        })
        .is_none()
    );
}
